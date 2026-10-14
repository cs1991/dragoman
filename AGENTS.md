# Agent Instructions for Dragoman

This document defines conventions, architecture rules, and git workflows for AI agents working in this repository.

---

## 1. Project Overview and Core Constraints

Dragoman is an OpenAI-compatible proxy server for Gemini Web. It routes requests directly to the Gemini 4 Argon model (`Argon4p0`) with a Google AI Pro subscription.

Non-negotiable architectural rules:
- Zero browser in production runtime: `dragoman serve` communicates strictly over native HTTP/2 with OpenSSL (`assistant.lamda.BardFrontendService/StreamGenerate`). The browser runs only once during `dragoman login` to capture session tokens.
- Native and stdlib first: Do not add external dependencies when a few lines of standard library code achieve the goal.
- Server reliability: Request handlers must return structured error responses. Never use `unwrap()` or `expect()` on user input in the HTTP path.

---

## 2. Secrets and Machine Data Discipline

Never commit local machine paths, usernames, or session tokens into git.

Protected items:
- `~/.config/dragoman/session.json`
- Anti-CSRF tokens (`SNlM0e`)
- Session IDs (`f_sid`)
- Build labels (`bl`)
- Google authentication cookies (`__Secure-*`, `HSID`, `SSID`)
- Personal email addresses or home paths

Before staging any commit, check your diff:
```bash
python3 ~/.agents/skills.disabled/repo-public-readiness/scripts/scan.py . --staged
```

---

## 3. Agentic Git and PR Workflow

Agents working on this repository must use branch-based changes and pull requests. Direct pushes to `master` are reserved for initial repository scaffolding.

### Step 1: Branch Creation
Create a branch named by category:
- `feat/<feature-name>` for new capabilities
- `fix/<issue-name>` for bug fixes
- `docs/<doc-topic>` for documentation changes

```bash
git checkout -b feat/your-feature-name
```

### Step 2: Implementation and Local Verification
Make focused changes. Verify code formatting, compilation, and tests before committing:
```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

### Step 3: Self-Check and Commit
Commit with conventional commit messages (`feat: ...`, `fix: ...`, `test: ...`, `docs: ...`):
```bash
git add <files>
git commit -m "feat: description of change"
```

### Step 4: Open Pull Request
Push the branch and open a pull request using the GitHub CLI:
```bash
git push -u origin feat/your-feature-name
gh pr create --title "feat: description" --body "Summary of changes and test results."
```

### Step 5: Peer Agent Review and Merge
In automated agent setups, a reviewer agent audits the pull request:
1. Verifies that tests pass in GitHub Actions CI.
2. Checks that diffs contain no hardcoded secrets or machine-bound paths.
3. Checks that markdown files follow anti-slop rules.
4. Once verified, the reviewer agent approves and merges:
```bash
gh pr review --approve -b "Verified tests pass and diff is clean."
gh pr merge --squash --delete-branch
```

### Step 6: Automated Issue Scanning
Agents should scan the repository periodically for:
- Dead code paths or obsolete dependencies.
- Changes in upstream Google Gemini RPC responses.
- Missing edge-case test coverage.

When found, open a tracked issue:
```bash
gh issue create --title "Investigate: <topic>" --body "Observed issue and suggested resolution."
```

---

## 4. Documentation and Writing Rules

When writing or editing markdown documents, follow the anti-slop guidelines:
- Avoid buzzwords: do not use seamless, cutting-edge, revolutionize, leverage, delve, robust, game-changer, or unlock.
- Punctuation in prose: do not use em dashes, en dashes, semicolons, exclamation marks, or mid-sentence colons.
- Ground statements in numbers, code identifiers, and measurable facts.
