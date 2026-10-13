"""Frontmatter lint for the plugin's skills: the one YAML mistake that has
broken a skill so far, not a YAML validator.

pi parses a skill's frontmatter with the npm `yaml` package, which rejects a
plain (unquoted) scalar containing ": " ("Nested mappings are not allowed in
compact mappings"); the openrouter skill's description once had
"(default: Xiaomi ...)" and pi refused to load it. This check flags such
single-line plain values; quoted values and block scalars pass unchecked, and
other YAML errors remain possible."""
import re
import unittest
from pathlib import Path

PLUGIN = Path(__file__).parent
KEY = re.compile(r"^([A-Za-z_][\w-]*):[ \t]+(.*)$")


def frontmatter(text):
    if not text.startswith("---\n"):
        return []
    end = text.find("\n---", 4)
    return text[4:end].splitlines() if end != -1 else []


def plain_values_with_colon(text):
    """(key, value) of top-level single-line plain scalars that contain ': '."""
    bad = []
    for line in frontmatter(text):
        m = KEY.match(line)
        if not m:
            continue
        key, value = m[1], m[2].rstrip()
        if not value or value[0] in "'\"|>[{&*!":
            continue  # quoted, block, flow or tagged: not a plain scalar
        if ": " in value or value.endswith(":"):
            bad.append((key, value))
    return bad


class SkillFrontmatterTest(unittest.TestCase):
    def test_the_lint_itself(self):
        broken = "---\nname: x\ndescription: Consult a model (default: MiMo) for a second opinion.\n---\n"
        fixed = "---\nname: x\ndescription: 'Consult a model (default: MiMo) for a second opinion.'\n---\n"
        self.assertEqual(plain_values_with_colon(broken), [("description", "Consult a model (default: MiMo) for a second opinion.")])
        self.assertEqual(plain_values_with_colon(fixed), [])

    def test_no_skill_has_a_colon_in_a_plain_value(self):
        skills = sorted(PLUGIN.glob("skills/*/SKILL.md"))
        self.assertTrue(skills)
        for skill in skills:
            with self.subTest(skill=skill.parent.name):
                self.assertEqual(plain_values_with_colon(skill.read_text()), [],
                                 "quote the value: pi's YAML parser rejects ': ' in a plain scalar")


if __name__ == "__main__":
    unittest.main()
