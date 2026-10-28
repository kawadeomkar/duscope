/*
 * gab_bench: what each getattrlistbulk(2) attribute set costs per entry on
 * macOS (P1-BENCH-02).
 *
 * It walks the given trees single-threaded, once per mode, with the page
 * cache warm, and reports the median nanoseconds per entry of each mode
 * over --reps rounds. The modes run interleaved, in a rotated order each
 * round, so drift in the machine's state spreads over all of them.
 *
 *   lean         name, type and allocated size: what a size-only walker reads
 *   basic        duscope's basic list (P1-WALK-03), in the proposal's order
 *   clones       basic + ATTR_CMNEXT_CLONEID | ATTR_CMNEXT_EXT_FLAGS
 *   privatesize  clones + ATTR_CMNEXT_PRIVATESIZE
 *   fstatat      readdir + fstatat(AT_SYMLINK_NOFOLLOW): the reference
 *
 * The decision rule (D23) compares clones with basic, and privatesize with
 * clones: each must add 10% or less to stay on by default (--clones) or ship
 * (PRIVATESIZE).
 *
 * Build and run:
 *   clang -O2 -Wall -Wextra -o gab_bench gab_bench.c
 *   ./gab_bench --reps 5 --json out.json TREE...
 *   ./gab_bench --make DIR FILES DIRS    # a tree of clones of one 64 KiB file
 */

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/attr.h>
#include <sys/clonefile.h>
#include <sys/stat.h>
#include <sys/sysctl.h>
#include <sys/utsname.h>
#include <sys/vnode.h>
#include <time.h>
#include <unistd.h>

#define BUFFER_BYTES (64 * 1024)
#define MAX_REPS 101

enum mode { LEAN, BASIC, CLONES, PRIVATESIZE, FSTATAT, MODES };
static const char *mode_names[MODES] = {"lean", "basic", "clones", "privatesize", "fstatat"};

static char buffer[BUFFER_BYTES] __attribute__((aligned(8)));

struct walk {
    uint64_t entries;
    uint64_t allocated;
};

static void request(enum mode mode, struct attrlist *attrs, uint64_t *options) {
    memset(attrs, 0, sizeof *attrs);
    attrs->bitmapcount = ATTR_BIT_MAP_COUNT;
    *options = FSOPT_PACK_INVAL_ATTRS;
    if (mode == LEAN) {
        attrs->commonattr = ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_OBJTYPE | ATTR_CMN_ERROR;
        attrs->fileattr = ATTR_FILE_ALLOCSIZE;
        return;
    }
    attrs->commonattr = ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_DEVID | ATTR_CMN_OBJTYPE |
                        ATTR_CMN_MODTIME | ATTR_CMN_CHGTIME | ATTR_CMN_FLAGS | ATTR_CMN_FILEID |
                        ATTR_CMN_ERROR;
    attrs->dirattr = ATTR_DIR_LINKCOUNT | ATTR_DIR_MOUNTSTATUS | ATTR_DIR_ALLOCSIZE | ATTR_DIR_DATALENGTH;
    attrs->fileattr = ATTR_FILE_LINKCOUNT | ATTR_FILE_ALLOCSIZE | ATTR_FILE_DATALENGTH | ATTR_FILE_DATAALLOCSIZE;
    if (mode == CLONES || mode == PRIVATESIZE) {
        attrs->forkattr = ATTR_CMNEXT_CLONEID | ATTR_CMNEXT_EXT_FLAGS;
        *options |= FSOPT_ATTR_CMN_EXTENDED;
    }
    if (mode == PRIVATESIZE) {
        attrs->forkattr |= ATTR_CMNEXT_PRIVATESIZE;
    }
}

/* The name and the object type of a record packed for `attrs`: the name's
 * attrreference follows the returned set (and the error, when returned),
 * and in every mode the device id, when requested, sits between it and the
 * object type. */
static int record_name_type(const char *record, const struct attrlist *attrs, const char **name,
                            fsobj_type_t *type) {
    const char *p = record + sizeof(uint32_t);
    attribute_set_t returned;
    memcpy(&returned, p, sizeof returned);
    p += sizeof returned;
    if (returned.commonattr & ATTR_CMN_ERROR) {
        uint32_t error;
        memcpy(&error, p, sizeof error);
        p += sizeof error;
        if (error != 0) {
            return -1;
        }
    }
    attrreference_t ref;
    memcpy(&ref, p, sizeof ref);
    *name = p + ref.attr_dataoffset;
    p += sizeof ref;
    if (attrs->commonattr & ATTR_CMN_DEVID) {
        p += sizeof(dev_t);
    }
    memcpy(type, p, sizeof *type);
    return 0;
}

static void walk_bulk(const char *path, enum mode mode, struct walk *w) {
    int fd = open(path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0) {
        return;
    }
    struct attrlist attrs;
    uint64_t options;
    request(mode, &attrs, &options);
    char **subdirs = NULL;
    size_t nsub = 0, capsub = 0;
    for (;;) {
        int count = getattrlistbulk(fd, &attrs, buffer, sizeof buffer, options);
        if (count <= 0) {
            break;
        }
        const char *record = buffer;
        for (int i = 0; i < count; i++) {
            uint32_t length;
            memcpy(&length, record, sizeof length);
            const char *name;
            fsobj_type_t type;
            if (record_name_type(record, &attrs, &name, &type) == 0) {
                w->entries++;
                if (type == VDIR) {
                    if (nsub == capsub) {
                        capsub = capsub ? capsub * 2 : 16;
                        subdirs = realloc(subdirs, capsub * sizeof *subdirs);
                    }
                    size_t len = strlen(path) + strlen(name) + 2;
                    subdirs[nsub] = malloc(len);
                    snprintf(subdirs[nsub], len, "%s/%s", path, name);
                    nsub++;
                }
            }
            record += length;
        }
    }
    close(fd);
    for (size_t i = 0; i < nsub; i++) {
        walk_bulk(subdirs[i], mode, w);
        free(subdirs[i]);
    }
    free(subdirs);
}

static void walk_fstatat(const char *path, struct walk *w) {
    int fd = open(path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0) {
        return;
    }
    DIR *dir = fdopendir(dup(fd));
    if (dir == NULL) {
        close(fd);
        return;
    }
    char **subdirs = NULL;
    size_t nsub = 0, capsub = 0;
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
            continue;
        }
        struct stat st;
        if (fstatat(fd, entry->d_name, &st, AT_SYMLINK_NOFOLLOW) != 0) {
            continue;
        }
        w->entries++;
        w->allocated += (uint64_t)st.st_blocks * 512;
        if (S_ISDIR(st.st_mode)) {
            if (nsub == capsub) {
                capsub = capsub ? capsub * 2 : 16;
                subdirs = realloc(subdirs, capsub * sizeof *subdirs);
            }
            size_t len = strlen(path) + strlen(entry->d_name) + 2;
            subdirs[nsub] = malloc(len);
            snprintf(subdirs[nsub], len, "%s/%s", path, entry->d_name);
            nsub++;
        }
    }
    closedir(dir);
    close(fd);
    for (size_t i = 0; i < nsub; i++) {
        walk_fstatat(subdirs[i], w);
        free(subdirs[i]);
    }
    free(subdirs);
}

static double now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC_RAW, &ts);
    return (double)ts.tv_sec * 1e9 + (double)ts.tv_nsec;
}

static int compare_double(const void *a, const void *b) {
    double x = *(const double *)a, y = *(const double *)b;
    return (x > y) - (x < y);
}

static double median(double *values, int n) {
    qsort(values, (size_t)n, sizeof *values, compare_double);
    return n % 2 ? values[n / 2] : (values[n / 2 - 1] + values[n / 2]) / 2;
}

static int make_tree(const char *root, long files, long dirs) {
    if (mkdir(root, 0755) != 0 && errno != EEXIST) {
        perror(root);
        return 1;
    }
    char source[4096];
    snprintf(source, sizeof source, "%s/source.bin", root);
    FILE *f = fopen(source, "wb");
    if (f == NULL) {
        perror(source);
        return 1;
    }
    for (int i = 0; i < 64 * 1024; i++) {
        fputc((i * 131) & 0xff, f);
    }
    fclose(f);
    for (long d = 0; d < dirs; d++) {
        char dir[4096];
        snprintf(dir, sizeof dir, "%s/d%ld", root, d);
        if (mkdir(dir, 0755) != 0 && errno != EEXIST) {
            perror(dir);
            return 1;
        }
    }
    for (long i = 0; i < files; i++) {
        char clone[4096];
        snprintf(clone, sizeof clone, "%s/d%ld/clone-%ld.bin", root, i % dirs, i);
        if (clonefile(source, clone, 0) != 0 && errno != EEXIST) {
            perror(clone);
            return 1;
        }
    }
    return 0;
}

static void usage(void) {
    fprintf(stderr, "usage: gab_bench [--reps N] [--json FILE] TREE...\n"
                    "       gab_bench --make DIR FILES DIRS\n");
    exit(2);
}

int main(int argc, char **argv) {
    int reps = 5;
    const char *json = NULL;
    int first = 1;
    while (first < argc && strncmp(argv[first], "--", 2) == 0) {
        if (strcmp(argv[first], "--make") == 0 && first + 3 < argc) {
            return make_tree(argv[first + 1], atol(argv[first + 2]), atol(argv[first + 3]));
        } else if (strcmp(argv[first], "--reps") == 0 && first + 1 < argc) {
            reps = atoi(argv[++first]);
        } else if (strcmp(argv[first], "--json") == 0 && first + 1 < argc) {
            json = argv[++first];
        } else {
            usage();
        }
        first++;
    }
    if (first >= argc || reps < 1 || reps > MAX_REPS) {
        usage();
    }

    FILE *out = json ? fopen(json, "w") : NULL;
    if (json && out == NULL) {
        perror(json);
        return 1;
    }
    struct utsname uts;
    uname(&uts);
    char cpu[256] = "";
    size_t cpu_len = sizeof cpu;
    sysctlbyname("machdep.cpu.brand_string", cpu, &cpu_len, NULL, 0);
    if (out) {
        fprintf(out, "{\"tool\":\"gab_bench\",\"darwin\":\"%s\",\"cpu\":\"%s\",\"reps\":%d,\"trees\":[", uts.release,
                cpu, reps);
    }

    for (int t = first; t < argc; t++) {
        const char *tree = argv[t];
        double ns[MODES][MAX_REPS];
        uint64_t entries[MODES] = {0};
        /* One unmeasured pass of every mode warms the caches. */
        for (int m = 0; m < MODES; m++) {
            struct walk w = {0, 0};
            if (m == FSTATAT) {
                walk_fstatat(tree, &w);
            } else {
                walk_bulk(tree, (enum mode)m, &w);
            }
            entries[m] = w.entries;
        }
        for (int r = 0; r < reps; r++) {
            for (int k = 0; k < MODES; k++) {
                int m = (k + r) % MODES;
                struct walk w = {0, 0};
                double start = now_ns();
                if (m == FSTATAT) {
                    walk_fstatat(tree, &w);
                } else {
                    walk_bulk(tree, (enum mode)m, &w);
                }
                ns[m][r] = (now_ns() - start) / (double)(w.entries ? w.entries : 1);
            }
        }
        double med[MODES];
        for (int m = 0; m < MODES; m++) {
            med[m] = median(ns[m], reps);
        }
        printf("%s (%llu entries)\n", tree, (unsigned long long)entries[BASIC]);
        for (int m = 0; m < MODES; m++) {
            printf("  %-12s %8.1f ns/entry  %+6.1f%% vs basic\n", mode_names[m], med[m],
                   100.0 * (med[m] / med[BASIC] - 1.0));
        }
        if (out) {
            fprintf(out, "%s{\"tree\":\"%s\",\"entries\":%llu,\"ns_per_entry\":{", t > first ? "," : "", tree,
                    (unsigned long long)entries[BASIC]);
            for (int m = 0; m < MODES; m++) {
                fprintf(out, "%s\"%s\":%.1f", m ? "," : "", mode_names[m], med[m]);
            }
            fprintf(out, "},\"clones_over_basic\":%.4f,\"privatesize_over_clones\":%.4f}",
                    med[CLONES] / med[BASIC] - 1.0, med[PRIVATESIZE] / med[CLONES] - 1.0);
        }
    }
    if (out) {
        fprintf(out, "]}\n");
        fclose(out);
    }
    return 0;
}
