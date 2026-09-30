#!/usr/bin/env python3
"""Tests for skills.py: python3 .github/scripts/test_skills.py"""

import pathlib
import sys
import tempfile
import textwrap
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import skills  # noqa: E402

TOP_HELP = """\
Usage: dre [OPTIONS] <COMMAND>

Commands:
  validate  Check the project
  run       Run reports: render, execute
  plugin    Manage plugins (sources, formats, destinations)
  help      Print this message

Options:
  -v, --verbose
          Show every step
      --log-level <LOG_LEVEL>
          How much to show
  -h, --help
          Print help (see a summary with '-h')
  -V, --version
          Print version
"""

RUN_HELP = """\
Usage: dre run [OPTIONS] [SELECTOR]...

Arguments:
  [SELECTOR]...  What to run

Options:
  -s, --select <SELECTOR>...
          What to select
      --preview [<ROWS>]
          Execute with a row limit
  -v, --verbose
          Show every step
"""

PLUGIN_HELP = """\
Usage: dre plugin [OPTIONS] <COMMAND>

Commands:
  list     List installed plugins
  install  Install a plugin from the registry
  help     Print this message

Options:
  -v, --verbose
          Show every step
"""

LEAF_HELP = """\
Usage: dre plugin list [OPTIONS]

Options:
  -v, --verbose
          Show every step
"""

HELPS = {
    (): TOP_HELP,
    ("run",): RUN_HELP,
    ("validate",): LEAF_HELP,
    ("plugin",): PLUGIN_HELP,
    ("plugin", "list"): LEAF_HELP,
    ("plugin", "install"): LEAF_HELP,
}


def cli():
    return skills.Cli(lambda path: HELPS[tuple(path)])


class HelpParsing(unittest.TestCase):
    def test_commands_and_flags(self):
        subs, flags = skills.parse_help(TOP_HELP)
        self.assertEqual(subs, {"validate", "run", "plugin", "help"})
        self.assertEqual(flags, {"-v", "--verbose", "--log-level", "-h", "--help", "-V", "--version"})

    def test_leaf_has_no_commands(self):
        subs, flags = skills.parse_help(RUN_HELP)
        self.assertEqual(subs, set())
        self.assertIn("--preview", flags)
        self.assertIn("-s", flags)


class Mentions(unittest.TestCase):
    def test_inline_code_and_blocks(self):
        md = textwrap.dedent("""\
            Run `dre validate` first, then:

            ```bash
            dre run monthly --preview 50   # a sample
            cd x && dre plugin list
            ```
            """)
        self.assertCountEqual(
            skills.dre_mentions(md),
            [["validate"], ["run", "monthly", "--preview", "50"], ["plugin", "list"]],
        )

    def test_ignores_names_that_only_contain_dre(self):
        md = "`dre_project.yml`, `~/.dre/profiles.yml`, `dre-plugin-duckdb`, `{{ dre_utils.star() }}`, dre in prose"
        self.assertEqual(skills.dre_mentions(md), [])

    def test_output_is_not_a_command(self):
        self.assertEqual(skills.dre_mentions("It prints `dre 0.1.0` or `dre <version>`"), [["<version>"]])

    def test_flag_with_value(self):
        self.assertEqual(skills.dre_mentions("`dre run --var day=2026-01-01`"), [["run", "--var", "day=2026-01-01"]])


class CheckMention(unittest.TestCase):
    def test_known_commands_and_flags_pass(self):
        c = cli()
        for tokens in (["validate"], ["run", "monthly", "--preview", "5"], ["plugin", "install", "xlsx"], ["--version"],
                       ["run", "-s", "daily", "-v"], ["plugin", "list", "--verbose"], ["run", "--preview=5"]):
            self.assertIsNone(c.check(tokens), tokens)

    def test_unknown_command(self):
        self.assertIn("`dre lint`", cli().check(["lint"]))

    def test_unknown_subcommand(self):
        self.assertIn("`dre plugin upgrade`", cli().check(["plugin", "upgrade", "x"]))

    def test_unknown_flag(self):
        self.assertIn("--dry", cli().check(["run", "--dry"]))

    def test_placeholders_are_skipped(self):
        self.assertIsNone(cli().check(["<command>", "--help"]))
        self.assertIsNone(cli().check(["run", "<report>"]))


class Ranges(unittest.TestCase):
    def test_parse_and_contains(self):
        r = skills.parse_range(">=0.1.0-rc.1, <0.2.0")
        self.assertTrue(skills.in_range("0.1.0-rc.1", r))
        self.assertTrue(skills.in_range("0.1.0", r))
        self.assertTrue(skills.in_range("0.1.7", r))
        self.assertFalse(skills.in_range("0.2.0", r))
        self.assertFalse(skills.in_range("0.2.0-rc.1", r))
        self.assertFalse(skills.in_range("0.0.1-alpha-12", r))

    def test_prerelease_ordering(self):
        v = skills.parse_version
        self.assertLess(v("0.1.0-rc.1"), v("0.1.0"))
        self.assertLess(v("0.1.0-rc.2"), v("0.1.0-rc.10"))
        self.assertLess(v("1.0.0-alpha"), v("1.0.0-rc.1"))

    def test_bad_range(self):
        with self.assertRaises(ValueError):
            skills.parse_range("0.1")
        with self.assertRaises(ValueError):
            skills.parse_range(">=0.1.0")  # needs an upper bound


class Frontmatter(unittest.TestCase):
    def test_parse(self):
        fm, body = skills.frontmatter(textwrap.dedent("""\
            ---
            name: dre-run
            description: Run a report. Use when the user says "run it".
            metadata:
              version: "1.0.0"
              dre: ">=0.1.0, <0.2.0"
            ---
            # Body
            """))
        self.assertEqual(fm["name"], "dre-run")
        self.assertEqual(fm["metadata"], {"version": "1.0.0", "dre": ">=0.1.0, <0.2.0"})
        self.assertEqual(body, "# Body\n")

    def test_plain_scalar_that_isnt_yaml(self):
        # An unquoted value holding `: ` or ` #` isn't valid YAML; other installers reject the skill.
        for bad in ('description: fixes "dre: command not found"', "description: use it # not a comment"):
            with self.assertRaises(ValueError):
                skills.frontmatter(f"---\nname: x\n{bad}\n---\n")
        fm, _ = skills.frontmatter('---\nname: x\ndescription: "fixes dre: command not found"\n---\n')
        self.assertEqual(fm["description"], "fixes dre: command not found")

    def test_missing(self):
        with self.assertRaises(ValueError):
            skills.frontmatter("# no frontmatter\n")

    def test_problems(self):
        self.assertEqual(skills.frontmatter_problems("dre-run", {"name": "dre-run", "description": "x"}), [])
        self.assertTrue(skills.frontmatter_problems("dre-run", {"name": "dre_run", "description": "x"}))
        self.assertTrue(skills.frontmatter_problems("dre-run", {"name": "dre-run"}))
        self.assertTrue(skills.frontmatter_problems("dre-run", {"name": "dre-run", "description": "x" * 1025}))


class Practices(unittest.TestCase):
    PRACTICES = textwrap.dedent("""\
        # Practices

        ### SEC-1: Never ask for a credential
        **Level:** block

        ### REP-2: Variables, not hardcoded dates
        **Level:** warn
        """)

    def test_defined(self):
        self.assertEqual(skills.defined_practices(self.PRACTICES), {"SEC-1", "REP-2"})

    def test_cited(self):
        self.assertEqual(skills.cited_practices("Per SEC-1 and (REP-2), not ISO-8601 or SEC-12x"), {"SEC-1", "REP-2"})


class PluginDocs(unittest.TestCase):
    DOCS = textwrap.dedent("""\
        # First-party plugins

        ## Sources

        ### `duckdb`

        | Field | Notes |
        |---|---|
        | `path` | Database file. |

        ## Destinations

        Intro for destinations.

        ### Several destinations

        Lists.

        ### `slack`

        The profile holds `token`.
        """)

    def test_section(self):
        self.assertIn("`path`", skills.doc_section(self.DOCS, "Sources", "`duckdb`"))
        self.assertNotIn("Destinations", skills.doc_section(self.DOCS, "Sources", "`duckdb`"))
        self.assertEqual(skills.doc_section(self.DOCS, "Destinations", None).strip(), "Intro for destinations.")
        with self.assertRaises(KeyError):
            skills.doc_section(self.DOCS, "Sources", "`nope`")

    def test_undocumented(self):
        fields = [{"name": "path", "description": "file"}, {"name": "threads", "description": "n"},
                  {"name": "memory_limit", "description": ""}]
        problems = skills.undocumented("source/duckdb", fields, "| `path` | Database file. |")
        self.assertEqual(len(problems), 2)
        self.assertTrue(any("`threads`" in p and "docs" in p for p in problems))
        self.assertTrue(any("`memory_limit`" in p and "description" in p for p in problems))


class PluginNames(unittest.TestCase):
    def test_unknown_types_and_formats(self):
        md = "```yaml\ndev: {type: postgres}\noutput: {format: xlsx}\nx: {type: mysql}\ny:\n  format: pdf\n```\n"
        self.assertEqual(skills.unknown_plugins(md, {"postgres", "xlsx"}), ["`type: mysql`", "`format: pdf`"])


class Serving(unittest.TestCase):
    def test_newest_stable_moves_skills_latest(self):
        self.assertTrue(skills.serves_latest("skills-v1.1.0", ["skills-v1.0.0", "skills-v1.1.0"]))

    def test_an_older_line_doesnt(self):
        self.assertFalse(skills.serves_latest("skills-v1.2.1", ["skills-v1.2.1", "skills-v2.0.0"]))

    def test_prerelease_only_before_the_first_stable(self):
        self.assertTrue(skills.serves_latest("skills-v1.0.0-rc.2", ["skills-v1.0.0-rc.1", "skills-v1.0.0-rc.2"]))
        self.assertFalse(skills.serves_latest("skills-v1.1.0-rc.1", ["skills-v1.0.0", "skills-v1.1.0-rc.1"]))

    def test_other_tags_are_ignored(self):
        self.assertTrue(skills.serves_latest("skills-v1.0.0", ["v0.1.0", "duckdb-v2.0.0", "skills-v1.0.0"]))


class Sync(unittest.TestCase):
    def test_inline_block(self):
        text = "a\n<!-- BEGIN shared/x.md -->\nold\n<!-- END shared/x.md -->\nb\n"
        out = skills.inline_shared(text, {"x.md": "new\nlines\n"})
        self.assertEqual(out, "a\n<!-- BEGIN shared/x.md -->\nnew\nlines\n<!-- END shared/x.md -->\nb\n")

    def test_unknown_block(self):
        with self.assertRaises(KeyError):
            skills.inline_shared("<!-- BEGIN shared/y.md -->\n<!-- END shared/y.md -->\n", {})

    def test_diff_tree(self):
        with tempfile.TemporaryDirectory() as a, tempfile.TemporaryDirectory() as b:
            pa, pb = pathlib.Path(a), pathlib.Path(b)
            (pa / "f.md").write_text("1")
            (pb / "f.md").write_text("1")
            self.assertEqual(skills.diff_tree(pa, pb), [])
            (pb / "f.md").write_text("2")
            (pb / "g.md").write_text("x")
            self.assertEqual(sorted(skills.diff_tree(pa, pb)), ["f.md", "g.md"])


if __name__ == "__main__":
    unittest.main()
