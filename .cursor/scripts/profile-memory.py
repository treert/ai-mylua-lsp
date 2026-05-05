#!/usr/bin/env python3
"""
Launch or attach to MyLua's Extension Development Host and summarize LSP memory.

The script enables MYLUA_MEM_PROFILE=1, waits for the server to log its Ready
memory census, samples RSS while indexing runs, and prints a compact report.
It uses only Python's standard library so it can run on macOS, Linux, and
Windows.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import subprocess
import sys
import time
from typing import Iterable
from urllib.parse import unquote, urlparse


SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
EXT_DIR = REPO_ROOT / "vscode-extension"
DEFAULT_TARGET = REPO_ROOT / "tests" / "lua-root"
EDH_MARKER = f"extensionDevelopmentPath={EXT_DIR}"


READY_RE = re.compile(
    r"workspace indexing complete: (?P<files>\d+) files \(Ready\) in (?P<total_ms>\d+) ms "
    r"\[scan=(?P<scan_ms>\d+) ms, parse=(?P<parse_ms>\d+) ms, merge=(?P<merge_ms>\d+) ms\]"
)
MEM_RE = re.compile(r"\[mem\] (?P<section>\w+): (?P<body>.*)$")
TOP_RE = re.compile(r"\[mem\] top_tree_file (?P<body>.*)$")
KEY_VALUE_RE = re.compile(r"(\w+)=(\d+)")
URI_PATH_RE = re.compile(r'path: "([^"]+)"')
URI_FIELD_RE = re.compile(r"uri=(\S+)")


@dataclass(frozen=True)
class RssStats:
    samples: int = 0
    current_mb: float = 0.0
    peak_mb: float = 0.0


def parse_int(value: str) -> int:
    return int(value.rstrip(","))


def parse_kv_body(body: str) -> dict[str, int]:
    return {key: parse_int(value) for key, value in KEY_VALUE_RE.findall(body)}


def parse_top_tree_file(body: str) -> dict[str, int | str]:
    row: dict[str, int | str] = parse_kv_body(body)
    match = URI_PATH_RE.search(body)
    if match:
        row["path"] = match.group(1)
        return row
    uri_match = URI_FIELD_RE.search(body)
    row["path"] = file_uri_to_path(uri_match.group(1)) if uri_match else body.split(" uri=", 1)[-1]
    return row


def file_uri_to_path(uri: str) -> str:
    parsed = urlparse(uri)
    if parsed.scheme != "file":
        return uri
    if platform.system() == "Windows" and parsed.netloc:
        return unquote(f"//{parsed.netloc}{parsed.path}")
    if platform.system() == "Windows" and re.match(r"^/[A-Za-z]:/", parsed.path):
        return unquote(parsed.path[1:])
    return unquote(parsed.path)


def parse_profile_log(text: str) -> dict[str, object]:
    profile: dict[str, object] = {"top_tree_files": []}
    for line in text.splitlines():
        ready = READY_RE.search(line)
        if ready:
            profile["ready"] = {
                key: int(value)
                for key, value in ready.groupdict().items()
            }
            continue

        top = TOP_RE.search(line)
        if top:
            profile.setdefault("top_tree_files", []).append(parse_top_tree_file(top.group("body")))
            continue

        mem = MEM_RE.search(line)
        if mem:
            profile[mem.group("section")] = parse_kv_body(mem.group("body"))
    return profile


def profile_complete(profile: dict[str, object], top_count: int) -> bool:
    required = {"ready", "documents", "summaries", "aggregation", "lua_symbols"}
    if not required.issubset(profile):
        return False
    return len(profile.get("top_tree_files", [])) >= min(top_count, 1)


def mib(value: int) -> float:
    return value / (1024 * 1024)


def seconds(ms: int) -> float:
    return ms / 1000.0


def fmt_count(value: int) -> str:
    return f"{value:,}"


def format_summary(profile: dict[str, object], rss: RssStats, top_count: int = 10) -> str:
    ready = profile.get("ready", {})
    docs = profile.get("documents", {})
    summaries = profile.get("summaries", {})
    aggregation = profile.get("aggregation", {})
    symbols = profile.get("lua_symbols", {})
    top_files = profile.get("top_tree_files", [])[:top_count]

    lines = [
        "MyLua LSP Memory Profile",
        "=" * 24,
    ]
    if ready:
        lines.append(
            f"Index Ready: {ready['files']} files in {seconds(ready['total_ms']):.2f}s "
            f"(scan {seconds(ready['scan_ms']):.2f}s, parse {seconds(ready['parse_ms']):.2f}s, "
            f"merge {seconds(ready['merge_ms']):.2f}s)"
        )
    lines.append(
        f"RSS: current {rss.current_mb:.1f} MB, peak {rss.peak_mb:.1f} MB "
        f"({rss.samples} samples)"
    )

    if docs:
        lines.extend(
            [
                "",
                "Documents",
                f"  Count: {fmt_count(docs['count'])}",
                f"  Source: {mib(docs['source_bytes']):.1f} MiB",
                f"  Line index: {mib(docs['line_index_bytes']):.1f} MiB "
                f"({fmt_count(docs['line_starts'])} starts)",
                f"  Tree nodes: {fmt_count(docs['tree_nodes'])}",
                f"  Scopes: {fmt_count(docs['scopes'])}, declarations: {fmt_count(docs['scope_decls'])}",
            ]
        )

    if summaries:
        lines.extend(
            [
                "",
                "Summaries",
                f"  Functions: {fmt_count(summaries['functions'])}",
                f"  Type defs / fields: {fmt_count(summaries['type_defs'])} / {fmt_count(summaries['type_fields'])}",
                f"  Table shapes / fields: {fmt_count(summaries['table_shapes'])} / {fmt_count(summaries['table_fields'])}",
                f"  Call sites: {fmt_count(summaries['call_sites'])}",
            ]
        )

    if aggregation:
        lines.extend(
            [
                "",
                "Aggregation",
                f"  Global nodes / candidates: {fmt_count(aggregation['global_nodes'])} / {fmt_count(aggregation['global_candidates'])}",
                f"  Type names / candidates: {fmt_count(aggregation['type_names'])} / {fmt_count(aggregation['type_candidates'])}",
                f"  Module entries: {fmt_count(aggregation['module_entries'])}",
            ]
        )

    if symbols:
        lines.extend(
            [
                "",
                "Lua Symbols",
                f"  Count: {fmt_count(symbols['count'])}",
                f"  String bytes: {mib(symbols['string_bytes']):.1f} MiB",
                f"  Arena allocated: {mib(symbols['arena_bytes']):.1f} MiB",
            ]
        )

    if top_files:
        lines.extend(["", f"Top {len(top_files)} Tree-Heavy Files"])
        for row in top_files:
            lines.append(
                f"  {row['rank']:>2}. {fmt_count(row['tree_nodes'])} nodes, "
                f"{mib(row['source_bytes']):.1f} MiB, "
                f"{fmt_count(row['scope_decls'])} decls - {row['path']}"
            )

    return "\n".join(lines)


def run_checked(cmd: list[str], cwd: Path, env: dict[str, str] | None = None) -> None:
    print(f"==> {' '.join(cmd)}")
    subprocess.run(cmd, cwd=str(cwd), env=env, check=True)


def build_env(profile: str) -> dict[str, str]:
    env = os.environ.copy()
    cargo_bin = Path.home() / ".cargo" / "bin"
    if cargo_bin.exists():
        env["PATH"] = f"{cargo_bin}{os.pathsep}{env.get('PATH', '')}"
    env["MYLUA_MEM_PROFILE"] = "1"
    env["MYLUA_LSP_BUILD"] = profile
    return env


def build_project(args: argparse.Namespace, profile: str, env: dict[str, str]) -> None:
    if args.skip_build or args.attach:
        print("==> Skipping build")
        return
    if not args.skip_lsp:
        cargo_args = ["cargo", "build"]
        if profile == "release":
            cargo_args.append("--release")
        run_checked(cargo_args, REPO_ROOT / "lsp", env)
    else:
        print("==> Skipping LSP build")

    if not args.skip_ext:
        run_checked(["npm", "run", "compile"], EXT_DIR, env)
    else:
        print("==> Skipping extension compile")


def find_editor(explicit: str | None = None) -> str:
    candidates = [explicit] if explicit else ["code", "cursor"]
    for name in candidates:
        if name and shutil.which(name):
            return name
    raise SystemExit("ERROR: Neither 'code' nor 'cursor' CLI found in PATH.")


def run_powershell(script: str) -> subprocess.CompletedProcess[str]:
    exe = shutil.which("powershell") or shutil.which("pwsh")
    if not exe:
        return subprocess.CompletedProcess([], 1, "", "PowerShell not found")
    return subprocess.run(
        [exe, "-NoProfile", "-Command", script],
        text=True,
        capture_output=True,
    )


def kill_existing_edh() -> None:
    print("==> Checking for existing Extension Development Host...")
    if platform.system() == "Windows":
        marker = EDH_MARKER.replace("'", "''")
        script = (
            "Get-CimInstance Win32_Process | "
            f"Where-Object {{ $_.ProcessId -ne $PID -and $_.CommandLine -and $_.CommandLine.Contains('{marker}') }} | "
            "ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }"
        )
        run_powershell(script)
        return

    proc = subprocess.run(["pgrep", "-f", EDH_MARKER], text=True, capture_output=True)
    pids = [int(line) for line in proc.stdout.splitlines() if line.strip().isdigit()]
    if not pids:
        print("    No existing instance found.")
        return
    print(f"    Terminating EDH PIDs: {' '.join(map(str, pids))}")
    for pid in pids:
        try:
            os.kill(pid, signal.SIGTERM)
        except OSError:
            pass
    time.sleep(2)


def launch_extension(target: Path, profile: str, env: dict[str, str], editor: str) -> None:
    kill_existing_edh()
    print(f"==> Launching Extension Development Host [{profile}]")
    print(f"    Extension: {EXT_DIR}")
    print(f"    Target: {target}")
    subprocess.Popen(
        [editor, f"--extensionDevelopmentPath={EXT_DIR}", str(target)],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def lsp_pids() -> list[int]:
    if platform.system() == "Windows":
        script = (
            "Get-Process mylua-lsp -ErrorAction SilentlyContinue | "
            "Sort-Object StartTime -Descending | ForEach-Object { $_.Id }"
        )
        proc = run_powershell(script)
        if proc.returncode != 0:
            return []
        return [int(line) for line in proc.stdout.splitlines() if line.strip().isdigit()]

    proc = subprocess.run(["pgrep", "-x", "mylua-lsp"], text=True, capture_output=True)
    if proc.returncode != 0:
        return []
    return [int(line) for line in proc.stdout.splitlines() if line.strip().isdigit()]


def latest_lsp_pid(exclude: set[int] | None = None) -> int | None:
    exclude = exclude or set()
    for pid in reversed(lsp_pids()):
        if pid not in exclude:
            return pid
    return None


def rss_mb(pid: int) -> float | None:
    if platform.system() == "Windows":
        proc = run_powershell(f"(Get-Process -Id {pid} -ErrorAction SilentlyContinue).WorkingSet64")
        text = proc.stdout.strip()
        return int(text) / (1024 * 1024) if proc.returncode == 0 and text.isdigit() else None

    proc = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], text=True, capture_output=True)
    text = proc.stdout.strip()
    return int(text.split()[0]) / 1024 if proc.returncode == 0 and text else None


def first_workspace_folder(workspace_file: Path) -> Path | None:
    try:
        data = json.loads(workspace_file.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    folders = data.get("folders")
    if not isinstance(folders, list) or not folders:
        return None
    first = folders[0]
    if not isinstance(first, dict):
        return None
    raw = first.get("path") or first.get("uri")
    if not isinstance(raw, str):
        return None
    if raw.startswith("file:"):
        return Path(file_uri_to_path(raw))
    path = Path(raw)
    return path if path.is_absolute() else (workspace_file.parent / path).resolve()


def default_log_path(target: Path) -> Path:
    if target.is_dir():
        base = target
    elif target.suffix == ".code-workspace":
        base = first_workspace_folder(target) or target.parent
    else:
        base = target.parent
    return base / ".vscode" / "mylua-lsp.log"


def prepare_log_for_launch(log_path: Path) -> None:
    try:
        log_path.unlink()
    except FileNotFoundError:
        return
    except OSError as exc:
        print(f"WARNING: could not remove stale profile log {log_path}: {exc}", file=sys.stderr)


def monitor(
    log_path: Path,
    timeout: float,
    interval: float,
    top_count: int,
    pid: int | None = None,
    exclude_pids: set[int] | None = None,
) -> tuple[dict[str, object], RssStats]:
    started = time.time()
    samples = 0
    current = 0.0
    peak = 0.0
    profile: dict[str, object] = {"top_tree_files": []}

    print(f"==> Waiting for profile log: {log_path}")
    while time.time() - started < timeout:
        sample_pid = pid or latest_lsp_pid(exclude_pids)
        if sample_pid is not None:
            sampled = rss_mb(sample_pid)
            if sampled is not None:
                current = sampled
                peak = max(peak, sampled)
                samples += 1

        if log_path.exists():
            profile = parse_profile_log(log_path.read_text(errors="replace"))
            if profile_complete(profile, top_count):
                return profile, RssStats(samples=samples, current_mb=current, peak_mb=peak)

        time.sleep(interval)

    raise SystemExit(f"ERROR: timed out waiting for memory profile in {log_path}")


def parse_args(argv: Iterable[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Profile MyLua LSP memory usage through Extension Development Host.")
    parser.add_argument("--target", type=Path, default=DEFAULT_TARGET, help="Workspace directory or .code-workspace to open.")
    parser.add_argument("--log", type=Path, help="Profile log path. Defaults to <target>/.vscode/mylua-lsp.log.")
    parser.add_argument("--debug", action="store_true", help="Use target/debug instead of the default release build.")
    parser.add_argument("--release", action="store_true", help="Use target/release (default). Kept for symmetry with test-extension.")
    parser.add_argument("--skip-build", action="store_true", help="Skip both LSP and extension build.")
    parser.add_argument("--skip-lsp", action="store_true", help="Skip only cargo build.")
    parser.add_argument("--skip-ext", action="store_true", help="Skip only npm run compile.")
    parser.add_argument("--attach", action="store_true", help="Do not build or launch; monitor an already running mylua-lsp.")
    parser.add_argument("--pid", type=int, help="Specific mylua-lsp PID to sample, useful with --attach.")
    parser.add_argument("--timeout", type=float, default=300.0, help="Seconds to wait for Ready memory profile.")
    parser.add_argument("--interval", type=float, default=0.5, help="RSS sampling interval in seconds.")
    parser.add_argument("--top", type=int, default=10, help="Number of top tree-heavy files to print.")
    parser.add_argument("--editor", help="Editor CLI to launch, e.g. code or cursor.")
    return parser.parse_args(list(argv))


def main(argv: Iterable[str] = sys.argv[1:]) -> int:
    args = parse_args(argv)
    profile = "debug" if args.debug else "release"
    target = args.target.resolve()
    if not target.exists():
        raise SystemExit(f"ERROR: target not found: {target}")

    env = build_env(profile)
    log_path = (args.log or default_log_path(target)).resolve()

    build_project(args, profile, env)
    existing_lsp_pids = set(lsp_pids())
    if not args.attach:
        prepare_log_for_launch(log_path)
        launch_extension(target, profile, env, find_editor(args.editor))
    else:
        print("==> Attach mode: monitoring existing mylua-lsp")

    exclude = set() if args.attach else existing_lsp_pids
    profile_log, rss = monitor(log_path, args.timeout, args.interval, args.top, args.pid, exclude)
    print()
    print(format_summary(profile_log, rss, args.top))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
