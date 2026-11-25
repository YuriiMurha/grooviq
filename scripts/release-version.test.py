import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("release_version", Path(__file__).with_name("release-version.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseVersionTests(unittest.TestCase):
    def test_matching_sources(self):
        versions = release.read_versions(Path(__file__).resolve().parents[1])
        self.assertEqual(release.validate("v" + versions["tauri"], versions), versions["tauri"])
        self.assertEqual(len(versions), 6)

    def test_each_mismatched_or_missing_source_fails(self):
        for key in release.read_versions(Path(__file__).resolve().parents[1]):
            with self.subTest(source=key):
                versions = dict.fromkeys(release.read_versions(Path(__file__).resolve().parents[1]), "0.1.0")
                versions[key] = "0.2.0"
                with self.assertRaises(ValueError):
                    release.validate("v0.1.0", versions)
        with self.assertRaises(ValueError):
            release.validate("v0.1.0", {})

    def test_bad_and_unsafe_tags(self):
        for tag in ["0.1.0", "v01.1.0", "v0.1.0-beta", "v0.1.0+meta", "v0.1.0\n", "v0.1.0;echo bad", "v$(id)", "v１.1.0", "v65536.1.0", "main", "v-1.0.0"]:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                release.validate(tag, {"app": tag[1:]})


if __name__ == "__main__":
    unittest.main()
