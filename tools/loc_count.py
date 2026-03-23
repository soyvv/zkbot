#!/usr/bin/env python3
"""Lines-of-code counter for the zkbot monorepo.

Groups code by module, language, and section (source vs test/bench/harness).
Outputs a table to stdout; use --json for machine-readable output.

Usage:
    python tools/loc_count.py            # summary table
    python tools/loc_count.py --detail   # per-module breakdown
    python tools/loc_count.py --json     # JSON output
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# ── Language detection ────────────────────────────────────────────────────────

LANG_BY_EXT: dict[str, str] = {
    ".rs": "Rust",
    ".py": "Python",
    ".java": "Java",
    ".proto": "Proto",
    ".sql": "SQL",
    ".sh": "Shell",
    ".toml": "Config",
    ".yaml": "Config",
    ".yml": "Config",
    ".json": "Config",
    ".md": "Docs",
}

SKIP_DIRS = {
    ".git", ".venv", ".idea", ".vscode", "__pycache__", "target",
    "node_modules", "dist", "build", ".worktrees", ".claude", ".codex",
    ".trade_doctor_generated", "wheels",
}

SKIP_FILES = {"uv.lock", "Cargo.lock"}

# ── Module mapping ────────────────────────────────────────────────────────────
# Each file is assigned to exactly one module based on its path.


def classify_module(rel: Path) -> str:
    """Return a module name for a file path relative to REPO_ROOT."""
    parts = rel.parts

    # rust/crates/<crate>/...
    if len(parts) >= 3 and parts[0] == "rust" and parts[1] == "crates":
        return f"rust/{parts[2]}"

    # rust/strategies/<strat>/...
    if len(parts) >= 3 and parts[0] == "rust" and parts[1] == "strategies":
        return f"rust/strategies/{parts[2]}"

    # rust/ top-level files (Cargo.toml, etc.)
    if parts[0] == "rust":
        return "rust/_root"

    # java/...
    if parts[0] == "java":
        return "java/pilot"

    # libs/<lib>/...
    if len(parts) >= 2 and parts[0] == "libs":
        return f"libs/{parts[1]}"

    # services/<svc>/...
    if len(parts) >= 2 and parts[0] == "services":
        return f"services/{parts[1]}"

    # venue-integrations/<venue>/...
    if len(parts) >= 2 and parts[0] == "venue-integrations":
        return f"venues/{parts[1]}"

    # protos/
    if parts[0] == "protos":
        return "protos"

    # devops/
    if parts[0] == "devops":
        return "devops"

    # scripts/
    if parts[0] == "scripts":
        return "scripts"

    # tools/
    if parts[0] == "tools":
        return "tools"

    # docs/
    if parts[0] == "docs":
        return "docs"

    # top-level files
    return "_root"


# ── Section classification (source / test / bench / harness) ──────────────────

def classify_section(rel: Path, lang: str) -> str:
    """Classify a file as 'source', 'test', 'bench', or 'harness'."""
    name = rel.name
    parts = rel.parts

    # Benchmarks
    if "bench" in name.lower() or "benches" in parts:
        return "bench"

    # Examples / e2e harness
    if "examples" in parts or "example" in parts:
        return "harness"

    # Test directories
    if "tests" in parts or "test" in parts:
        return "test"

    # Java test convention
    if lang == "Java":
        if "src/test" in str(rel):
            return "test"
        if name.endswith("Test.java") or name.endswith("IT.java"):
            return "test"

    # Python test convention
    if lang == "Python":
        if name.startswith("test_") or name.endswith("_test.py"):
            return "test"
        if name == "conftest.py":
            return "test"

    # Rust test convention (inline tests counted as source — we can't split without parsing)
    # But files in tests/ directory are caught above
    if lang == "Rust" and name.startswith("test_"):
        return "test"

    # SQL: migration-test or test seeds
    if lang == "SQL" and "test" in str(rel).lower():
        return "test"

    # DevOps init scripts are harness
    if "init" in parts and lang == "SQL":
        return "harness"

    # Dockerfiles are harness
    if name.startswith("Dockerfile"):
        return "harness"

    return "source"


# ── Counting ──────────────────────────────────────────────────────────────────

@dataclass
class Bucket:
    files: int = 0
    lines: int = 0
    blank: int = 0
    comment: int = 0

    @property
    def code(self) -> int:
        return self.lines - self.blank - self.comment

    def add(self, other: "Bucket") -> None:
        self.files += other.files
        self.lines += other.lines
        self.blank += other.blank
        self.comment += other.comment


COMMENT_PREFIXES: dict[str, tuple[str, ...]] = {
    "Rust": ("//", "///", "//!"),
    "Python": ("#",),
    "Java": ("//", "*", "/**"),
    "Proto": ("//",),
    "SQL": ("--",),
    "Shell": ("#",),
    "Config": ("#",),
    "Docs": (),  # don't count comment lines for docs
}


def count_file(path: Path, lang: str) -> Bucket:
    """Count lines in a single file."""
    b = Bucket(files=1)
    prefixes = COMMENT_PREFIXES.get(lang, ())
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except (OSError, UnicodeDecodeError):
        return b

    for line in text.splitlines():
        b.lines += 1
        stripped = line.strip()
        if not stripped:
            b.blank += 1
        elif prefixes and any(stripped.startswith(p) for p in prefixes):
            b.comment += 1

    return b


# key: (module, lang, section)
Stats = dict[tuple[str, str, str], Bucket]


def walk_repo() -> Stats:
    """Walk the repo and collect stats."""
    stats: Stats = defaultdict(Bucket)

    for path in REPO_ROOT.rglob("*"):
        if not path.is_file():
            continue

        rel = path.relative_to(REPO_ROOT)

        # Skip excluded dirs
        if any(p in SKIP_DIRS for p in rel.parts):
            continue

        if rel.name in SKIP_FILES:
            continue

        ext = path.suffix.lower()

        # Handle Makefile and Dockerfile specially
        if rel.name == "Makefile":
            lang = "Config"
        elif rel.name.startswith("Dockerfile"):
            lang = "Config"
        elif ext in LANG_BY_EXT:
            lang = LANG_BY_EXT[ext]
        else:
            continue  # skip unknown extensions

        module = classify_module(rel)
        section = classify_section(rel, lang)
        bucket = count_file(path, lang)
        stats[(module, lang, section)].add(bucket)

    return stats


# ── Output formatting ─────────────────────────────────────────────────────────

def print_summary(stats: Stats) -> None:
    """Print a summary grouped by language and section."""
    # Aggregate by (lang, section)
    by_lang_section: dict[tuple[str, str], Bucket] = defaultdict(Bucket)
    for (mod, lang, section), b in stats.items():
        by_lang_section[(lang, section)].add(b)

    # Sort: source languages first, then by code lines desc
    lang_order = ["Rust", "Python", "Java", "Proto", "SQL", "Shell", "Config", "Docs"]
    section_order = ["source", "test", "bench", "harness"]

    print()
    print("=" * 78)
    print(f"{'Language':<10} {'Section':<10} {'Files':>7} {'Lines':>9} {'Code':>9} {'Comment':>9} {'Blank':>7}")
    print("-" * 78)

    grand = Bucket()
    for lang in lang_order:
        lang_total = Bucket()
        for section in section_order:
            key = (lang, section)
            if key not in by_lang_section:
                continue
            b = by_lang_section[key]
            lang_total.add(b)
            print(f"{lang:<10} {section:<10} {b.files:>7} {b.lines:>9,} {b.code:>9,} {b.comment:>9,} {b.blank:>7,}")
        if lang_total.files > 0:
            print(f"{'':10} {'SUBTOTAL':<10} {lang_total.files:>7} {lang_total.lines:>9,} {lang_total.code:>9,} {lang_total.comment:>9,} {lang_total.blank:>7,}")
            print()
            grand.add(lang_total)

    print("-" * 78)
    print(f"{'TOTAL':<10} {'':10} {grand.files:>7} {grand.lines:>9,} {grand.code:>9,} {grand.comment:>9,} {grand.blank:>7,}")
    print("=" * 78)


def print_detail(stats: Stats) -> None:
    """Print per-module detail."""
    # Group by module
    by_module: dict[str, dict[tuple[str, str], Bucket]] = defaultdict(lambda: defaultdict(Bucket))
    for (mod, lang, section), b in stats.items():
        by_module[mod][(lang, section)].add(b)

    # Sort modules alphabetically
    print()
    print("=" * 90)
    print(f"{'Module':<30} {'Lang':<8} {'Section':<10} {'Files':>6} {'Lines':>8} {'Code':>8} {'Comment':>8} {'Blank':>6}")
    print("-" * 90)

    grand = Bucket()
    for mod in sorted(by_module):
        mod_total = Bucket()
        entries = by_module[mod]
        for (lang, section) in sorted(entries):
            b = entries[(lang, section)]
            mod_total.add(b)
            print(f"{mod:<30} {lang:<8} {section:<10} {b.files:>6} {b.lines:>8,} {b.code:>8,} {b.comment:>8,} {b.blank:>6,}")
        if len(entries) > 1:
            print(f"{'':<30} {'':8} {'subtotal':<10} {mod_total.files:>6} {mod_total.lines:>8,} {mod_total.code:>8,} {mod_total.comment:>8,} {mod_total.blank:>6,}")
        print()
        grand.add(mod_total)

    print("-" * 90)
    print(f"{'TOTAL':<30} {'':8} {'':10} {grand.files:>6} {grand.lines:>8,} {grand.code:>8,} {grand.comment:>8,} {grand.blank:>6,}")
    print("=" * 90)


def output_json(stats: Stats) -> None:
    """Output stats as JSON."""
    rows = []
    for (mod, lang, section), b in sorted(stats.items()):
        rows.append({
            "module": mod,
            "language": lang,
            "section": section,
            "files": b.files,
            "lines": b.lines,
            "code": b.code,
            "comment": b.comment,
            "blank": b.blank,
        })
    json.dump(rows, sys.stdout, indent=2)
    print()


# ── Main ──────────────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(description="Count lines of code in zkbot repo")
    parser.add_argument("--detail", action="store_true", help="Show per-module breakdown")
    parser.add_argument("--json", action="store_true", help="Output as JSON")
    args = parser.parse_args()

    stats = walk_repo()

    if args.json:
        output_json(stats)
    elif args.detail:
        print_detail(stats)
    else:
        print_summary(stats)


if __name__ == "__main__":
    main()
