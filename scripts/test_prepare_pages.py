import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

from prepare_pages import BASE_URL, prepare


class PreparePagesTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def write(self, relative: str, document: str) -> None:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(document, encoding="utf-8")

    def test_adds_self_canonicals_and_discovery_files(self):
        self.write("index.html", "<!doctype html><html><head></head><body>Home</body></html>")
        self.write("guide/index.html", "<html><head></head><body>Guide</body></html>")
        self.write("guide/setup.html", "<html><head></head><body>Setup</body></html>")

        self.assertEqual(prepare(self.root), 3)

        self.assertIn(f'href="{BASE_URL}"', (self.root / "index.html").read_text())
        self.assertIn(f'href="{BASE_URL}guide/"', (self.root / "guide/index.html").read_text())
        self.assertIn(f'href="{BASE_URL}guide/setup.html"', (self.root / "guide/setup.html").read_text())
        locations = [node.text for node in ET.parse(self.root / "sitemap.xml").iter() if node.tag.endswith("loc")]
        self.assertEqual(locations, [BASE_URL, BASE_URL + "guide/", BASE_URL + "guide/setup.html"])
        self.assertEqual(
            (self.root / "robots.txt").read_text(),
            f"User-agent: *\nAllow: /\nSitemap: {BASE_URL}sitemap.xml\n",
        )

    def test_preserves_noindex_page_without_publishing_it(self):
        document = '<html><head><base href="/"><meta name="robots" content="nofollow, noindex"></head></html>'
        self.write("404.html", document)

        self.assertEqual(prepare(self.root), 0)

        prepared = (self.root / "404.html").read_text()
        self.assertIn('<base href="/glomeris/">', prepared)
        self.assertIn('content="nofollow, noindex"', prepared)
        self.assertNotIn("canonical", prepared)
        self.assertNotIn("404.html", (self.root / "sitemap.xml").read_text())

    def test_marks_generated_not_found_page_noindex(self):
        self.write("404.html", '<html><head><base href="/"></head><body>Missing</body></html>')

        prepare(self.root)

        prepared = (self.root / "404.html").read_text()
        self.assertIn('<base href="/glomeris/">', prepared)
        self.assertIn('<meta name="robots" content="noindex">', prepared)

    def test_replaces_one_existing_canonical_and_is_idempotent(self):
        self.write(
            "index.html",
            '<html><head><link href="https://wrong.example/" rel="canonical"></head></html>',
        )

        prepare(self.root)
        prepare(self.root)

        document = (self.root / "index.html").read_text()
        self.assertEqual(document.count('rel="canonical"'), 1)
        self.assertIn(f'href="{BASE_URL}"', document)

    def test_rejects_ambiguous_multiple_canonicals(self):
        self.write(
            "index.html",
            '<link rel="canonical" href="https://one.example/">'
            '<link rel="canonical" href="https://two.example/">',
        )

        with self.assertRaisesRegex(ValueError, "more than one canonical"):
            prepare(self.root)

    def test_normalizes_rustdoc_generated_navigation(self):
        self.write(
            "rustdoc/item.html",
            '<html><head></head><body><a href="javascript:void(0)">Back</a>'
            '<a href="../src/lib.rs.html#12-19">Source</a>'
            '<a href="#non-numeric-range">Section</a></body></html>',
        )

        prepare(self.root)

        document = (self.root / "rustdoc/item.html").read_text()
        self.assertIn('href="./index.html"', document)
        self.assertIn('href="../src/lib.rs.html#12"', document)
        self.assertIn('href="#non-numeric-range"', document)

    def test_supplies_an_empty_missing_rustdoc_implementor_index(self):
        self.write(
            "rustdoc/glomeris/trait.Example.html",
            '<html><head></head><script src="../trait.impl/glomeris/trait.Example.js"></script></html>',
        )

        prepare(self.root)

        support = self.root / "rustdoc/trait.impl/glomeris/trait.Example.js"
        self.assertEqual(support.read_text(), "// Rustdoc emitted no public implementors.\n")

    def test_rejects_a_rustdoc_implementor_index_outside_the_artifact(self):
        self.write(
            "rustdoc/item.html",
            '<html><head></head><script src="../../../trait.impl/escape.js"></script></html>',
        )

        with self.assertRaises(ValueError):
            prepare(self.root)

    def test_does_not_materialize_an_external_rustdoc_script(self):
        self.write(
            "rustdoc/item.html",
            '<html><head></head><script src="https://cdn.example/trait.impl/item.js"></script></html>',
        )

        prepare(self.root)

        self.assertFalse((self.root / "rustdoc/https:").exists())


if __name__ == "__main__":
    unittest.main()
