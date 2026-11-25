"""Exercise the real hook against staged content in disposable repositories."""

from pathlib import Path
import subprocess
import tempfile
import unittest


HOOK = Path(__file__).resolve().parents[1] / ".githooks" / "pre-commit"
CONFLICT = "<<<<<<< ours\nname: ours\n=======\nname: theirs\n>>>>>>> theirs\n"


class PreCommitTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.env = {
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "HOME": str(self.repo),
            "LANG": "C",
            "DEVELOPER_DIR": "/Library/Developer/CommandLineTools",
            "GIT_CONFIG_NOSYSTEM": "1",
        }
        self.git("init", "--quiet")

    def git(self, *args):
        return subprocess.run(
            ["git", *args], cwd=self.repo, env=self.env,
            capture_output=True, text=True, check=True,
        )

    def stage(self, content):
        (self.repo / "workflow.yml").write_text(content)
        self.git("add", "--", "workflow.yml")

    def hook(self):
        return subprocess.run(
            ["/bin/sh", str(HOOK)], cwd=self.repo, env=self.env,
            capture_output=True, text=True, timeout=10,
        )

    def test_staged_yaml_conflict_is_rejected(self):
        self.stage(CONFLICT)
        result = self.hook()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("workflow.yml", result.stdout + result.stderr)

    def test_unstaged_conflict_does_not_reject_clean_index(self):
        self.stage("name: clean\n")
        (self.repo / "workflow.yml").write_text(CONFLICT)
        self.assertEqual(self.hook().returncode, 0)

    def test_clean_worktree_does_not_hide_staged_conflict(self):
        self.stage(CONFLICT)
        (self.repo / "workflow.yml").write_text("name: clean\n")
        self.assertNotEqual(self.hook().returncode, 0)

    def test_normal_staged_content_is_accepted_without_changes(self):
        self.stage("name: clean\n")
        index_before = (self.repo / ".git" / "index").read_bytes()
        result = self.hook()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual((self.repo / ".git" / "index").read_bytes(), index_before)
        self.assertEqual((self.repo / "workflow.yml").read_text(), "name: clean\n")


if __name__ == "__main__":
    unittest.main()
