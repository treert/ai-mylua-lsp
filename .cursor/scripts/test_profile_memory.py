import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("profile-memory.py")


def load_script_module():
    spec = importlib.util.spec_from_file_location("profile_memory", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


SAMPLE_LOG = """
[01:22:26.051] [mylua-lsp] workspace indexing complete: 21173 files (Ready) in 9638 ms [scan=135 ms, parse=9298 ms, merge=179 ms]
[01:22:26.196] [mem] documents: count=21172 source_bytes=217160763 line_starts=5534275 line_index_bytes=44274200 tree_nodes=42935910 scopes=864511 scope_decls=1064804 scope_child_links=843339
[01:22:26.198] [mem] summaries: count=21172 globals=107791 functions=317337 function_name_index=60057 type_defs=48533 type_fields=507996 table_shapes=136753 table_fields=329578 call_sites=1040955
[01:22:26.198] [mem] aggregation: global_roots=21081 global_nodes=106968 global_candidates=107791 global_reverse_paths=107791 type_names=47794 type_candidates=48533 module_last_segments=20463 module_entries=21173 require_aliases=0
[01:22:26.198] [mem] lua_symbols: count=973093 string_bytes=28996639 arena_bytes=268431360
[01:22:26.202] [mem] top_tree_file rank=1 tree_nodes=863086 source_bytes=2354009 scope_decls=7 scopes=1 line_starts=25341 uri=Uri(Uri { scheme: "file", authority: Some(Authority { userinfo: None, host: "", host_parsed: RegName(""), port: None }), path: "/tmp/work/LetsGo.Script/Export/PBMessageMap.lua", query: None, fragment: None })
"""


class ProfileMemoryTests(unittest.TestCase):
    def test_parse_profile_log_extracts_ready_mem_and_top_files(self):
        module = load_script_module()

        profile = module.parse_profile_log(SAMPLE_LOG)

        self.assertEqual(profile["ready"]["files"], 21173)
        self.assertEqual(profile["ready"]["total_ms"], 9638)
        self.assertEqual(profile["documents"]["tree_nodes"], 42935910)
        self.assertEqual(profile["summaries"]["call_sites"], 1040955)
        self.assertEqual(profile["lua_symbols"]["arena_bytes"], 268431360)
        self.assertEqual(profile["top_tree_files"][0]["path"], "/tmp/work/LetsGo.Script/Export/PBMessageMap.lua")

    def test_format_summary_prints_human_readable_units(self):
        module = load_script_module()
        profile = module.parse_profile_log(SAMPLE_LOG)

        output = module.format_summary(
            profile,
            module.RssStats(samples=7, current_mb=3066.3, peak_mb=3297.8),
        )

        self.assertIn("Index Ready: 21173 files in 9.64s", output)
        self.assertIn("RSS: current 3066.3 MB, peak 3297.8 MB", output)
        self.assertIn("Source: 207.1 MiB", output)
        self.assertIn("Tree nodes: 42,935,910", output)
        self.assertIn("PBMessageMap.lua", output)

    def test_parse_profile_log_accepts_stable_uri_field(self):
        module = load_script_module()
        log = """
[01:22:26.202] [mem] top_tree_file rank=1 tree_nodes=10 source_bytes=20 scope_decls=1 scopes=1 line_starts=2 uri=file:///C:/work/foo.lua
"""

        original_system = module.platform.system
        module.platform.system = lambda: "Windows"
        try:
            profile = module.parse_profile_log(log)
        finally:
            module.platform.system = original_system

        self.assertEqual(profile["top_tree_files"][0]["path"], "C:/work/foo.lua")

    def test_prepare_log_for_launch_removes_stale_log(self):
        module = load_script_module()
        with tempfile.TemporaryDirectory() as tmp:
            log_path = Path(tmp) / "mylua-lsp.log"
            log_path.write_text(SAMPLE_LOG, encoding="utf-8")

            module.prepare_log_for_launch(log_path)

            self.assertFalse(log_path.exists())


if __name__ == "__main__":
    unittest.main()
