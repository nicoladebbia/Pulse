"""Tests for the night team's rules, guard and page.  python3 -m unittest discover -s scripts/team"""

import datetime as dt
import importlib
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import guard  # noqa: E402
import page  # noqa: E402
import rules  # noqa: E402

OWNER = "nicola"


def pr(n, **over):
    base = {"number": n, "title": f"PR {n}", "author": {"login": OWNER}, "isCrossRepository": False,
            "isDraft": False, "baseRefName": "main", "headRefName": f"feat/{n}", "headRefOid": f"sha{n}",
            "labels": [], "createdAt": f"2026-10-{n:02d}", "body": ""}
    base.update(over)
    return base


class ProtectedFiles(unittest.TestCase):
    def test_normal_code_is_fine(self):
        changes = [("M", "crates/pulse-fetcher/src/main.rs"), ("A", "migrations/060_new.sql"), ("M", "CLAUDE.md")]
        self.assertEqual(rules.protected_violations(changes), [])

    def test_gate_and_install_files_are_protected(self):
        for path in [".github/workflows/ci.yml", "scripts/team/rules.py", "launchd/com.pulse.live-signals.plist",
                     "scripts/scheduled-fetch.sh", "src-tauri/tauri.conf.json", "src-tauri/src/services/go_live.rs",
                     ".env", "ios/.env.local", "rust-toolchain.toml"]:
            self.assertTrue(rules.protected_violations([("M", path)]), path)

    def test_existing_migration_may_not_change(self):
        self.assertTrue(rules.protected_violations([("M", "migrations/042_x.sql")]))
        self.assertTrue(rules.protected_violations([("D", "migrations/042_x.sql")]))

    def test_rename_counts_as_delete_plus_add(self):
        changes = rules.parse_name_status("R100\0migrations/042_x.sql\0migrations/061_x.sql\0M\0src/a.ts\0")
        self.assertEqual(changes, [("D", "migrations/042_x.sql"), ("A", "migrations/061_x.sql"), ("M", "src/a.ts")])
        self.assertEqual(len(rules.protected_violations(changes)), 1)

    def test_rename_out_of_a_protected_path_is_caught(self):
        changes = rules.parse_name_status("R090\0.github/workflows/ci.yml\0docs/ci.yml\0")
        self.assertTrue(rules.protected_violations(changes))

    def test_unusual_file_names_are_still_matched(self):
        # Without -z git prints this as "scripts/team/\303\251 x.py" (quoted), which matched nothing.
        changes = rules.parse_name_status("A\0scripts/team/\u00e9 x.py\0M\0src/tab\there.ts\0")
        self.assertEqual(changes, [("A", "scripts/team/\u00e9 x.py"), ("M", "src/tab\there.ts")])
        self.assertEqual(len(rules.protected_violations(changes)), 1)

    def test_claude_folder_is_protected(self):
        for path in [".claude/settings.json", ".claude/skills/run-pulse/cdp.mjs"]:
            self.assertTrue(rules.protected_violations([("M", path)]), path)


class LiveEndpoint(unittest.TestCase):
    def test_only_added_live_host_is_flagged(self):
        self.assertTrue(rules.forbidden_content('+const URL: &str = "https://api.alpaca.markets/v2";'))
        self.assertEqual(rules.forbidden_content('-const URL: &str = "https://api.alpaca.markets/v2";'), [])
        self.assertEqual(rules.forbidden_content('+let u = "https://paper-api.alpaca.markets/v2";'), [])
        self.assertEqual(rules.forbidden_content('+let u = "https://data.alpaca.markets/v2/stocks";'), [])
        self.assertEqual(rules.forbidden_content("+++ b/api.alpaca.markets.rs"), [])


class PullRequests(unittest.TestCase):
    LABELLED = [{"name": "needs-you"}]

    def test_owner_pr_is_eligible(self):
        self.assertIsNone(rules.pr_skip_reason(pr(1), OWNER, "main", "needs-you"))

    def test_strangers_forks_drafts_and_labelled_are_skipped(self):
        cases = [pr(1, author={"login": "mallory"}), pr(1, isCrossRepository=True), pr(1, isDraft=True),
                 pr(1, baseRefName="release"), pr(1, labels=self.LABELLED), pr(1, author=None),
                 pr(1, busy="it has commits on this Mac that are not pushed yet")]
        for case in cases:
            self.assertIsNotNone(rules.pr_skip_reason(case, OWNER, "main", "needs-you"))

    def test_a_new_commit_lifts_the_teams_own_label(self):
        state = {}
        rules.record_attempt(state, "pr-1", "sha1", "blocked", parked=True)
        same = pr(1, labels=self.LABELLED)
        self.assertIsNotNone(rules.pr_skip_reason(same, OWNER, "main", "needs-you", state))
        moved = pr(1, labels=self.LABELLED, headRefOid="newsha")
        self.assertIsNone(rules.pr_skip_reason(moved, OWNER, "main", "needs-you", state))

    def test_a_label_added_by_hand_holds_until_removed(self):
        state = {}
        rules.record_attempt(state, "pr-1", "oldsha", "error")  # an old record, not a park
        moved = pr(1, labels=self.LABELLED, headRefOid="newsha")
        self.assertIsNotNone(rules.pr_skip_reason(moved, OWNER, "main", "needs-you", state))
        self.assertIsNotNone(rules.pr_skip_reason(moved, OWNER, "main", "needs-you", {}))


class Attempts(unittest.TestCase):
    def test_stuck_only_on_the_same_commit(self):
        state = {}
        rules.record_attempt(state, "main", "abc", "needs_user")
        self.assertIsNotNone(rules.stuck(state, "main", "abc"))
        self.assertIsNone(rules.stuck(state, "main", "def"))  # a new commit resets it
        self.assertIsNone(rules.error_cap(state, "main", "abc"))

    def test_errors_retry_three_times(self):
        state = {}
        for _ in range(2):
            rules.record_attempt(state, "pr-1", "abc", "error")
        self.assertIsNone(rules.error_cap(state, "pr-1", "abc"))
        rules.record_attempt(state, "pr-1", "abc", "error")
        self.assertIsNotNone(rules.error_cap(state, "pr-1", "abc"))
        self.assertIsNone(rules.error_cap(state, "pr-1", "def"))
        rules.clear_attempt(state, "pr-1")
        self.assertIsNone(rules.error_cap(state, "pr-1", "abc"))


class Ideas(unittest.TestCase):
    TEXT = "# Pulse ideas\n\n- [x] done thing — merged\n- [ ] dark mode toggle\n- plain line idea\n- [?] unclear — which one?\n"

    def test_parse(self):
        got = [(i["state"], i["text"]) for i in rules.parse_ideas(self.TEXT)]
        self.assertEqual(got, [("done", "done thing — merged"), ("open", "dark mode toggle"),
                               ("open", "plain line idea"), ("waiting", "unclear — which one?")])

    def test_mark_only_touches_the_matching_open_line(self):
        out = rules.mark_idea(self.TEXT, "dark mode toggle", "x", "merged 2026-11-14 (PR #60)")
        self.assertIn("- [x] dark mode toggle — merged 2026-11-14 (PR #60)\n", out)
        self.assertIn("- plain line idea\n", out)
        self.assertTrue(out.endswith("\n"))
        out = rules.mark_idea(self.TEXT, "plain line idea", "?", "needs a decision")
        self.assertIn("- [?] plain line idea — needs a decision", out)

    def test_slug_and_key(self):
        self.assertEqual(rules.slug("Add a Dark-Mode toggle!!"), "add-a-dark-mode-toggle")
        self.assertLessEqual(len(rules.slug("word " * 40)), 40)
        self.assertEqual(rules.slug("???"), "idea")
        long_a, long_b = "word " * 20 + "one", "word " * 20 + "two"  # same first words
        self.assertNotEqual(rules.idea_key(long_a), rules.idea_key(long_b))
        self.assertEqual(rules.idea_key(long_a), rules.idea_key(long_a))
        self.assertRegex(rules.idea_key("Dark mode"), r"^idea-dark-mode-[0-9a-f]{6}$")


class Queue(unittest.TestCase):
    def select(self, main_ok=True, prs=(), ideas=(), state=None, main_sha="m1"):
        return rules.select_jobs(main_ok, list(prs), list(ideas), state or {}, OWNER, "main", "needs-you", main_sha)

    def test_red_main_goes_first_and_alone(self):
        jobs, _ = self.select(main_ok=False, prs=[pr(1)], ideas=[{"text": "x", "state": "open"}])
        self.assertEqual([(j["kind"], j["sha"]) for j in jobs], [("main", "m1")])

    def test_red_main_is_not_retried_on_the_same_commit(self):
        state = {}
        rules.record_attempt(state, "main", "m1", "blocked")
        jobs, skipped = self.select(main_ok=False, prs=[pr(1)], state=state)
        self.assertEqual(jobs, [])
        self.assertEqual(skipped[0]["kind"], "main")
        jobs, _ = self.select(main_ok=False, state=state, main_sha="m2")  # main moved: try again
        self.assertEqual(len(jobs), 1)

    def test_red_main_continues_an_open_fix_instead_of_a_second_one(self):
        fix = pr(7, headRefName="fix/night-main-1113")
        jobs, _ = self.select(main_ok=False, prs=[pr(1), fix])
        self.assertEqual([j["key"] for j in jobs], ["pr-7"])
        parked = pr(7, headRefName="fix/night-main-1113", labels=[{"name": "needs-you"}])
        jobs, skipped = self.select(main_ok=False, prs=[pr(1), parked])
        self.assertEqual(jobs, [])
        self.assertEqual(skipped[0]["pr"], 7)
        stranger = pr(8, headRefName="fix/night-main-1113", author={"login": "mallory"})
        jobs, _ = self.select(main_ok=False, prs=[stranger])
        self.assertEqual([j["kind"] for j in jobs], ["main"])

    def test_open_prs_oldest_first_then_ideas(self):
        ideas = [{"text": "done", "state": "done"}, {"text": "next idea", "state": "open"}]
        jobs, skipped = self.select(prs=[pr(9), pr(3), pr(5, isDraft=True)], ideas=ideas)
        self.assertEqual([j["key"] for j in jobs], ["pr-3", "pr-9", rules.idea_key("next idea")])
        self.assertEqual([s["pr"] for s in skipped], [5])
        self.assertEqual(jobs[0]["sha"], "sha3")
        self.assertFalse(jobs[0]["labelled"])

    def test_memory_of_failures(self):
        state = {}
        for _ in range(3):
            rules.record_attempt(state, "pr-1", "sha1", "error")
        rules.record_attempt(state, "pr-2", "sha2", "blocked", parked=True)  # label since removed: try again
        for _ in range(3):
            rules.record_attempt(state, rules.idea_key("flaky"), "", "error")
        jobs, skipped = self.select(prs=[pr(1), pr(2)], ideas=[{"text": "flaky", "state": "open"}], state=state)
        self.assertEqual([j["key"] for j in jobs], ["pr-2"])
        self.assertEqual([s["why"] for s in skipped], ["failed 3 nights in a row"] * 2)


class Ci(unittest.TestCase):
    def test_states(self):
        ok = {"status": "COMPLETED", "conclusion": "SUCCESS"}
        self.assertEqual(rules.ci_state([]), "none")
        self.assertEqual(rules.ci_state(None), "none")
        self.assertEqual(rules.ci_state([ok, {"status": "COMPLETED", "conclusion": "SKIPPED"}]), "pass")
        self.assertEqual(rules.ci_state([ok, {"status": "IN_PROGRESS", "conclusion": ""}]), "pending")
        self.assertEqual(rules.ci_state([ok, {"status": "COMPLETED", "conclusion": "FAILURE"}]), "fail")
        self.assertEqual(rules.ci_state([{"status": "COMPLETED", "conclusion": "CANCELLED"}]), "fail")
        self.assertEqual(rules.ci_state([{"state": "PENDING"}]), "pending")
        self.assertEqual(rules.ci_state([{"state": "ERROR"}]), "fail")


class Guard(unittest.TestCase):
    REPO = os.path.expanduser("~/dev/Pulse")
    WT = REPO + "/.claude/worktrees/team-pr-1"

    def blocked(self, cmd):
        return guard.bash_block_reason(cmd, self.REPO)

    def test_everyday_commands_pass(self):
        for cmd in ["cargo test --workspace --locked", "git status && git commit -am 'fix'", "git stash push -u -m x",
                    "pnpm check", "gh pr view 53 --json title", "gh pr diff 53", "gh run view 1 --log-failed",
                    f"cd {self.WT} && cargo build -p pulse-fetcher",
                    './target/debug/pulse-fetcher --mode learn --db-path "$PULSE_DB_PATH"',
                    "cat .env.example", "grep -rn 'process.env' src", "source .venv/bin/activate",
                    "node .claude/skills/run-pulse/cdp.mjs shot /tmp/a.png", "git merge origin/main",
                    "cargo build --manifest-path src-tauri/Cargo.toml", "git log --oneline origin/main..HEAD"]:
            self.assertIsNone(self.blocked(cmd), cmd)

    def test_publishing_is_blocked(self):
        for cmd in ["git push", "git push origin HEAD:main", "git -C /x push --force", "cd x && git push -u origin b",
                    "gh pr merge 53 --merge", "gh pr create --fill", "gh release create v1", "gh api repos/x/y/merges",
                    "gh pr edit 1 --add-label x", "gh workflow run ci.yml", "gh secret set X",
                    'bash -c "git push origin HEAD"', "sh -c 'git push'", "/usr/bin/git push", "git --no-pager push",
                    "git -p push origin", 'bash -c "gh pr merge 5"', "/opt/homebrew/bin/gh pr merge 5"]:
            self.assertIsNotNone(self.blocked(cmd), cmd)

    def test_live_data_secrets_and_the_mac_are_blocked(self):
        for cmd in ["./target/debug/pulse-fetcher --mode daily", "cargo run -p pulse-fetcher -- --mode auto-trade",
                    "cat .env", f"cat {self.REPO}/.env", "cp .env.local /tmp/x", "node --env-file=.env x.js",
                    "sqlite3 ~/Library/Application\\ Support/com.pulse.app/pulse.db .tables",
                    "ls ~/Library/Mobile\\ Documents/", "launchctl list", "sudo rm x", "osascript -e 'x'",
                    "bash scripts/install-app.sh", f"cd {self.REPO} && git checkout main",
                    "git -C ~/dev/Pulse checkout main", "ls $HOME/dev/Pulse/src", "crontab -l",
                    "cat ~/.ssh/id_ed25519", "cat ~/.tauri/pulse-updater.key", "echo '{}' > .claude/settings.json"]:
            self.assertIsNotNone(self.blocked(cmd), cmd)

    def test_writes_stay_in_the_worktree(self):
        self.assertIsNone(guard.write_block_reason("src/a.ts", self.WT))
        self.assertIsNone(guard.write_block_reason("/tmp/notes.txt", self.WT))
        self.assertIsNotNone(guard.write_block_reason(self.REPO + "/src/a.ts", self.WT))
        self.assertIsNotNone(guard.write_block_reason("../../../src/a.ts", self.WT))
        self.assertIsNotNone(guard.write_block_reason(".env", self.WT))
        self.assertIsNone(guard.write_block_reason(".env.example", self.WT))
        self.assertIsNotNone(guard.write_block_reason(".claude/settings.json", self.WT))
        self.assertIsNotNone(guard.write_block_reason(".claude/skills/run-pulse/SKILL.md", self.WT))

    def test_writes_from_a_subfolder_use_the_worktree_root(self):
        sub = self.WT + "/src-tauri"
        self.assertIsNone(guard.write_block_reason("../README.md", sub, self.WT))
        self.assertIsNotNone(guard.write_block_reason("../.claude/settings.json", sub, self.WT))
        self.assertIsNotNone(guard.write_block_reason("../../team-pr-2/a.ts", sub, self.WT))

    def test_reads_of_secrets_live_data_and_the_main_checkout_are_blocked(self):
        home = os.path.expanduser("~")
        for path in [".env", "ios/.env.local", self.REPO + "/.env", self.REPO + "/src/a.ts", self.REPO,
                     home + "/Library/Application Support/com.pulse.app/pulse.db",
                     home + "/Library/Application Support/com.pulse.app/.env",
                     home + "/Library/Mobile Documents/com~apple~CloudDocs/Pulse/pulse-snapshot.json",
                     "~/.ssh/id_ed25519", home + "/.tauri/pulse-updater.key", home + "/.config/gh/hosts.yml"]:
            self.assertIsNotNone(guard.read_block_reason(path, self.WT, self.REPO), path)
        for path in ["src/a.ts", ".env.example", self.WT + "/CLAUDE.md", "/etc/hosts", "",
                     home + "/.cargo/registry/src/x/lib.rs", self.REPO + "/.claude/worktrees/team-pr-2/a.ts"]:
            self.assertIsNone(guard.read_block_reason(path, self.WT, self.REPO), path)

    def test_decide_routes_by_tool(self):
        def decide(tool, **args):
            return guard.decide({"tool_name": tool, "tool_input": args, "cwd": self.WT}, self.REPO, self.WT)

        self.assertIsNotNone(decide("Bash", command="git push"))
        self.assertIsNone(decide("Bash", command="cargo test"))
        self.assertIsNone(decide("Read", file_path="/etc/hosts"))
        self.assertIsNotNone(decide("Read", file_path=self.REPO + "/.env"))
        self.assertIsNotNone(decide("Grep", pattern="KEY", path=self.REPO))
        self.assertIsNone(decide("Grep", pattern="KEY"))
        self.assertIsNotNone(decide("Glob", pattern="*", path="~/.ssh"))
        self.assertIsNotNone(decide("Write", file_path="/etc/hosts"))
        self.assertIsNone(decide("Edit", file_path="src/a.ts"))
        self.assertIsNotNone(decide("mcp__claude_ai_Gmail__send_message", to="x"))
        self.assertIsNotNone(decide("CronCreate", prompt="x"))
        self.assertIsNone(decide("Agent", prompt="x"))
        self.assertIsNone(decide("WebSearch", query="x"))


class Page(unittest.TestCase):
    REPORT = {
        "started": "2026-11-14T04:15", "minutes": 62, "cost": 3.5, "repo": "nicola/Pulse",
        "main": {"sha": "abc1234", "ok": True, "failed_step": ""},
        "jobs": [
            {"kind": "pr", "title": "Scan <all> tickers", "pr": 54, "result": "merged", "summary": "It now scans & more.",
             "try_it": ["Open Signals"], "not_verified": ["live run"], "reasons": [], "minutes": 30},
            {"kind": "idea", "title": "Dark mode", "pr": 60, "result": "needs_user", "summary": "",
             "reasons": ["changes a protected file: .github/workflows/ci.yml"], "minutes": 20},
        ],
        "skipped": [{"kind": "pr", "pr": 7, "title": "Old", "why": "marked as draft"},
                    {"kind": "main", "title": "Fix failing checks on main", "why": "no change since the last attempt"}],
        "ideas": [{"text": "Next thing", "state": "open"}, {"text": "Unclear — which?", "state": "waiting"}],
        "unreleased": {"tag": "v0.8.7", "merges": 2}, "notes": [],
    }

    def test_render_escapes_and_shows_everything(self):
        html = page.render([self.REPORT, dict(self.REPORT, started="2026-11-13T04:15")])
        self.assertIn("<title>Pulse Night Team</title>", html)
        self.assertIn("Scan &lt;all&gt; tickers", html)
        self.assertNotIn("<all>", html)
        self.assertIn("https://github.com/nicola/Pulse/pull/54", html)
        for text in ["Needs you", "changes a protected file", "Next thing", "Earlier nights", "release Pulse", "marked as draft"]:
            self.assertIn(text, html)

    def test_empty_history_still_renders(self):
        self.assertIn("Nothing to work on", page.render([]))
        self.assertIn("Nothing to work on", page.as_text({}))

    def test_needs_you_lists_what_waits(self):
        waiting = page.needs_you(self.REPORT)
        # the stopped job, the stuck main fix, the waiting idea, the unreleased merges
        self.assertEqual(len(waiting), 4)
        self.assertTrue(any(w.startswith("Fix failing checks on main") for w in waiting), waiting)
        red = dict(self.REPORT, main={"ok": False, "failed_step": "clippy"}, jobs=[])
        self.assertIn("main is failing", page.needs_you(red)[0])
        fixed = dict(red, jobs=[{"kind": "pr", "pr": 9, "title": "fix", "result": "merged"}])
        self.assertFalse(any("main is failing" in w for w in page.needs_you(fixed)))


class Runner(unittest.TestCase):
    """team.py's bookkeeping around a job, against a temporary home folder."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.old_home = os.environ.get("PULSE_TEAM_HOME")
        os.environ["PULSE_TEAM_HOME"] = cls.tmp.name
        cls.team = importlib.import_module("team")
        importlib.reload(cls.team)  # pick up the temporary home even if already imported

    @classmethod
    def tearDownClass(cls):
        if cls.old_home is None:
            os.environ.pop("PULSE_TEAM_HOME", None)
        else:
            os.environ["PULSE_TEAM_HOME"] = cls.old_home
        cls.tmp.cleanup()

    def setUp(self):
        self.team.IDEAS.write_text("# Pulse ideas\n\n- [ ] dark mode\n- [ ] second idea\n")
        self.job = {"kind": "idea", "key": rules.idea_key("dark mode"), "title": "dark mode", "sha": ""}
        self.today = dt.date.today().isoformat()

    def res(self, result, **over):
        return dict({"result": result, "reasons": [], "pr": None}, **over)

    def ideas(self):
        return self.team.IDEAS.read_text()

    def test_merged_idea_is_ticked(self):
        state = {"attempts": {"pr-60": {"sha": "x", "result": "error", "tries": 1}}}
        self.team.after_job(self.job, self.res("merged", pr=60, pushed="abc"), state)
        self.assertIn(f"- [x] dark mode — merged {self.today} (PR #60)\n", self.ideas())
        self.assertIn("- [ ] second idea\n", self.ideas())
        self.assertEqual(state["attempts"], {})

    def test_idea_that_became_a_pr_is_not_built_twice(self):
        state = {}
        stopped = self.res("blocked", pr=60, pushed="abc", parked=True, reasons=["still not right"])
        self.team.after_job(self.job, stopped, state)
        self.assertIn(f"- [x] dark mode — now PR #60 ({self.today})\n", self.ideas())
        self.assertEqual(state["attempts"]["pr-60"], {"sha": "abc", "result": "blocked", "tries": 1, "parked": True})
        self.assertEqual([i["text"] for i in rules.parse_ideas(self.ideas()) if i["state"] == "open"], ["second idea"])

    def test_idea_with_a_question_waits_with_the_question(self):
        self.team.after_job(self.job, self.res("needs_user", reasons=["Which colours?\nsecond line"]), {})
        self.assertIn("- [?] dark mode — Which colours? second line\n", self.ideas())

    def test_idea_errors_retry_three_nights_then_wait(self):
        state = {}
        for _ in range(2):
            self.team.after_job(self.job, self.res("error", reasons=["timed out after 75 min"]), state)
            self.assertIn("- [ ] dark mode\n", self.ideas())
        self.team.after_job(self.job, self.res("error", reasons=["timed out after 75 min"]), state)
        self.assertIn("- [?] dark mode — failed 3 nights in a row: timed out after 75 min\n", self.ideas())
        self.assertEqual(state["attempts"], {})  # setting the line back to [ ] starts over

    def test_pr_and_main_outcomes_are_remembered_by_commit(self):
        state = {}
        job = {"kind": "pr", "key": "pr-5", "title": "t", "pr": 5, "sha": "origin-head"}
        self.team.after_job(job, self.res("blocked", pr=5), state)
        self.assertEqual(state["attempts"]["pr-5"]["sha"], "origin-head")
        self.team.after_job(job, self.res("needs_user", pr=5, pushed="new-head", parked=True), state)
        self.assertEqual(state["attempts"]["pr-5"], {"sha": "new-head", "result": "needs_user", "tries": 1, "parked": True})
        main = {"kind": "main", "key": "main", "title": "Fix main", "sha": "main-sha"}
        self.team.after_job(main, self.res("blocked"), state)
        self.assertIsNotNone(rules.stuck(state, "main", "main-sha"))
        self.team.after_job(job, self.res("ready", pr=5), state)
        self.assertNotIn("pr-5", state["attempts"])
        self.assertEqual(self.ideas().count("[ ]"), 2)  # non-idea jobs never touch the list

    def test_session_environment_cannot_trade_spend_or_publish(self):
        os.environ["ANTHROPIC_API_KEY"] = "real"
        os.environ["ALPACA_API_KEY"] = "real"
        try:
            env = self.team.night_env(self.team.HOME / "wt")
        finally:
            del os.environ["ANTHROPIC_API_KEY"], os.environ["ALPACA_API_KEY"]
        self.assertNotIn("ANTHROPIC_API_KEY", env)  # or `claude` itself would log in with it
        for key in ["ALPACA_API_KEY", "ALPACA_SECRET_KEY", "GROQ_API_KEY", "GH_TOKEN"]:
            self.assertEqual(env[key], "night-team-placeholder", key)
        self.assertEqual((env["AUTO_TRADE_ENABLED"], env["EXIT_DRY_RUN"], env["PULSE_LLM"]), ("false", "true", "local"))
        self.assertEqual(env["PULSE_DB_PATH"], str(self.team.DB_JOB))  # set even when no copy exists
        self.assertEqual(env["GIT_CONFIG_COUNT"], "2")
        self.assertIn("pushInsteadOf", env["GIT_CONFIG_KEY_0"])
        self.assertEqual(env["PULSE_TEAM_WORKTREE"], str(self.team.HOME / "wt"))
        hooks = self.team.guard_settings()["hooks"]["PreToolUse"]
        self.assertEqual(hooks[0]["matcher"], "*")  # every tool call, not only Bash


if __name__ == "__main__":
    unittest.main()
