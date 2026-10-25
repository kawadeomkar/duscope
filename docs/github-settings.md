# GitHub settings

This is the checklist of repository settings duscope depends on, from the "CI/CD"
section of the specification (package P0-CI-01). Agents never change repository
settings. Each unticked item carries the exact command that applies it: the author
runs the command and ticks the item. An item owned by another package names that
package instead.

The state below was read from the live repository on 2026-10-25. The commands
under [Auditing](#auditing) read it again.

## Branch protection on `main`

- [x] Pull requests are required before merging, with 0 approvals. There is one
  maintainer: agents open the pull requests, and the author's merge is the review.
- [x] Administrators are included (`enforce_admins`), so nobody bypasses the rules.
- [x] Linear history is required.
- [x] Force pushes and deletion of `main` are blocked.
- [x] Required status checks, with "Require branches to be up to date before
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

  Applied with all 20 checks, `schemas` included, on 2026-10-25.

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
- [x] Protection for `v*` tags (P0-CI-02): a tag ruleset under which only a
  repository administrator, the author, may create, move or delete a `v*` tag.
  Tags are never moved or deleted (a pulled release burns its number), and the
  `GITHUB_TOKEN` cannot create one. release.yml's `verify-tag` job checks the
  signature as well, so a tag pushed past the rule still releases nothing.
  `actor_id` 5 is the built-in admin role. Applied on 2026-10-24 as ruleset
  25776511, "Release tags".

  ```sh
  gh api -X POST repos/kawadeomkar/duscope/rulesets --input - <<'EOF'
  {
    "name": "Release tags",
    "target": "tag",
    "enforcement": "active",
    "conditions": { "ref_name": { "include": ["refs/tags/v*"], "exclude": [] } },
    "rules": [{ "type": "creation" }, { "type": "update" }, { "type": "deletion" }],
    "bypass_actors": [{ "actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always" }]
  }
  EOF
  ```

- [x] Protection for the `bench-results` branch (P0-BENCH-02): a branch ruleset
  under which nobody, administrators included, may delete the branch or rewrite its
  history. The nightly benchmark job only adds commits to it, so its pushes pass.
  Applied on 2026-10-25 as ruleset 25818907, "bench-results".

  ```sh
  gh api -X POST repos/kawadeomkar/duscope/rulesets --input - <<'EOF'
  {
    "name": "bench-results",
    "target": "branch",
    "enforcement": "active",
    "conditions": { "ref_name": { "include": ["refs/heads/bench-results"], "exclude": [] } },
    "rules": [{ "type": "deletion" }, { "type": "non_fast_forward" }]
  }
  EOF
  ```

## Changing the required checks

A renamed job, a new matrix entry or a new required job changes the check names.
Update the PUT above in the same pull request:
`p0_ci_01_settings_require_exactly_the_required_checks` works out the name each
job reports from its `name:` and matrix, and fails unless they are exactly the
checks in the PUT. When the author runs the PUT depends on the change:

- A check is added: after the merge. Until then branch protection does not wait
  for the new check.
- A check is renamed or removed: just before merging that pull request. Branch
  protection waits for every name in the PUT, and the pull request reports only
  the new names, so with the old settings it could never merge (administrators
  are included). Other open pull requests then wait on the new names until they
  are updated with `main`, which `strict` asks of them anyway.

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
