# GitHub settings

This is the checklist of repository settings duscope depends on, from the "CI/CD"
section of the specification (package P0-CI-01). Agents never change repository
settings. Each unticked item carries the exact command that applies it: the author
runs the command and ticks the item. An item owned by another package names that
package instead.

The state below was read from the live repository on 2026-10-22. The commands
under [Auditing](#auditing) read it again.

## Branch protection on `main`

- [x] Pull requests are required before merging, with 0 approvals. There is one
  maintainer: agents open the pull requests, and the author's merge is the review.
- [x] Administrators are included (`enforce_admins`), so nobody bypasses the rules.
- [x] Linear history is required.
- [x] Force pushes and deletion of `main` are blocked.
- [ ] Required status checks, with "Require branches to be up to date before
  merging" (`strict`). The checks are the jobs of
  [ci.yml](../.github/workflows/ci.yml) that the spec marks required:
  - `fmt`
  - `clippy` on the three runners
  - `test` on the three runners, plus its locale leg on each
  - `msrv (pinned)` and `msrv (stable)`
  - `deny`
  - `docs`
  - `schemas`
  - `size` on `macos-26` and `ubuntu-24.04`
  - `build-time`
  - `repro`
  - `changelog`

  `intel-mac` (best effort) and `size-comment` (it only posts the size) are not
  required. This PUT replaces the whole protection of `main`, so it restates the
  settings above unchanged. `app_id` 15368 is GitHub Actions, so only a workflow
  run can report these checks.

  Unticked until the PUT is run again: P0-REPO-05 added `schemas`, and the
  other 19 checks are already applied.

  ```sh
  gh api -X PUT repos/kawadeomkar/duscope/branches/main/protection --input - <<'EOF'
  {
    "required_status_checks": {
      "strict": true,
      "checks": [
        { "context": "fmt", "app_id": 15368 },
        { "context": "clippy (macos-26)", "app_id": 15368 },
        { "context": "clippy (ubuntu-24.04)", "app_id": 15368 },
        { "context": "clippy (ubuntu-24.04-arm)", "app_id": 15368 },
        { "context": "test (macos-26, default)", "app_id": 15368 },
        { "context": "test (ubuntu-24.04, default)", "app_id": 15368 },
        { "context": "test (ubuntu-24.04-arm, default)", "app_id": 15368 },
        { "context": "test (macos-26, locale)", "app_id": 15368 },
        { "context": "test (ubuntu-24.04, locale)", "app_id": 15368 },
        { "context": "test (ubuntu-24.04-arm, locale)", "app_id": 15368 },
        { "context": "msrv (pinned)", "app_id": 15368 },
        { "context": "msrv (stable)", "app_id": 15368 },
        { "context": "deny", "app_id": 15368 },
        { "context": "docs", "app_id": 15368 },
        { "context": "schemas", "app_id": 15368 },
        { "context": "size (macos-26)", "app_id": 15368 },
        { "context": "size (ubuntu-24.04)", "app_id": 15368 },
        { "context": "build-time", "app_id": 15368 },
        { "context": "repro", "app_id": 15368 },
        { "context": "changelog", "app_id": 15368 }
      ]
    },
    "enforce_admins": true,
    "required_pull_request_reviews": {
      "dismiss_stale_reviews": false,
      "require_code_owner_reviews": false,
      "require_last_push_approval": false,
      "required_approving_review_count": 0
    },
    "restrictions": null,
    "required_linear_history": true,
    "allow_force_pushes": false,
    "allow_deletions": false,
    "block_creations": false,
    "required_conversation_resolution": false,
    "lock_branch": false,
    "allow_fork_syncing": false
  }
  EOF
  ```

- [x] Merge queue: replaced by the strict required checks above. GitHub offers
  merge queues only to repositories owned by an organisation, and this one belongs
  to a personal account. With `strict`, a pull request merges only after its checks
  have passed on a branch that is up to date with `main`, which is the guarantee
  the queue would give. The cost is an "Update branch" and a second CI run when
  `main` has moved. The author decided this on 2026-10-22, and ci.yml has no
  `merge_group` trigger.

## Merge methods

- [x] Squash merging only. Merge commits and rebase merging are off, so each
  package lands on `main` as one commit.

## Actions

- [x] The `GITHUB_TOKEN` is read-only by default and cannot approve pull requests.
  Workflows widen it per job; in ci.yml only the size comment gets
  `pull-requests: write`.
- [x] Actions must be pinned to a full commit SHA. ci.yml already pins every
  action, and a `p0_ci_01_*` test checks that. With this setting GitHub also
  refuses an unpinned action in any other workflow, such as release.yml (P0-CI-02).

  ```sh
  gh api -X PUT repos/kawadeomkar/duscope/actions/permissions -F enabled=true -f allowed_actions=all -F sha_pinning_required=true
  ```

- [x] Workflows from fork pull requests wait for approval, for every outside
  contributor. Today only first-time contributors wait.

  ```sh
  gh api -X PUT repos/kawadeomkar/duscope/actions/permissions/fork-pr-contributor-approval -f approval_policy=all_external_contributors
  ```

## Security

- [x] Private vulnerability reporting, which `SECURITY.md` points reporters at.

  ```sh
  gh api -X PUT repos/kawadeomkar/duscope/private-vulnerability-reporting
  ```

- [x] Dependabot alerts.

  ```sh
  gh api -X PUT repos/kawadeomkar/duscope/vulnerability-alerts
  ```

- [x] Secret scanning, with push protection.

## Labels

- [x] The `no-changelog` label. The author sets it on a pull request with nothing
  user-visible, and the `changelog` check then accepts the pull request without a
  `changelog.d/<ID>.md` fragment.

  ```sh
  gh label create no-changelog --repo kawadeomkar/duscope --color cfd3d7 --description "Nothing user-visible: no changelog.d fragment needed"
  ```

## Owned by other packages

- [x] GitHub Pages served from the `gh-pages` branch, for the apt repository:
  P0-REPO-03. Served at https://kawadeomkar.github.io/duscope/ with HTTPS
  enforced; the branch holds a placeholder page until apt.yml publishes `/apt`.
- [ ] Protection for `v*` tags, so that only the author can push a release tag:
  P0-CI-02. GitHub now does this with a tag ruleset.

## Changing the required checks

A renamed job, a new matrix entry or a new required job changes the check names.
Update the PUT above in the same pull request; the `p0_ci_01_*` tests compare it
with ci.yml. The author runs the PUT again after the merge. Until then, branch
protection waits on a check name that no job reports any more, and every pull
request stays blocked.

## Auditing

```sh
gh api repos/kawadeomkar/duscope/branches/main/protection
gh api repos/kawadeomkar/duscope --jq '{allow_squash_merge, allow_merge_commit, allow_rebase_merge, security_and_analysis}'
gh api repos/kawadeomkar/duscope/actions/permissions
gh api repos/kawadeomkar/duscope/actions/permissions/workflow
gh api repos/kawadeomkar/duscope/actions/permissions/fork-pr-contributor-approval
gh api repos/kawadeomkar/duscope/private-vulnerability-reporting
gh api repos/kawadeomkar/duscope/vulnerability-alerts --silent && echo "Dependabot alerts: on"
gh api repos/kawadeomkar/duscope/rulesets
gh label list --repo kawadeomkar/duscope
```
