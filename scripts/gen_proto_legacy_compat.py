from __future__ import annotations

import ast
import re
import sys
from dataclasses import dataclass
from pathlib import Path


COMPAT_START = "# legacy-betterproto-compat-start"
COMPAT_END = "# legacy-betterproto-compat-end"


@dataclass
class EnumSpec:
    name: str
    values: list[str]


@dataclass
class ProtoSpec:
    package: str
    type_names: list[str]
    enums: list[EnumSpec]


def strip_line_comment(line: str) -> str:
    if "//" not in line:
        return line
    return line.split("//", 1)[0]


def parse_proto(proto_path: Path) -> ProtoSpec:
    package = ""
    type_names: list[str] = []
    enums: list[EnumSpec] = []
    current_enum: EnumSpec | None = None
    block_stack: list[str] = []

    for raw_line in proto_path.read_text(encoding="utf-8").splitlines():
        line = strip_line_comment(raw_line).strip()
        if not line:
            continue

        package_match = re.match(r"package\s+([A-Za-z_][\w.]*)\s*;", line)
        if package_match:
            package = package_match.group(1)

        open_count = line.count("{")
        close_count = line.count("}")

        decl_match = re.match(r"(message|enum|service)\s+([A-Za-z_]\w*)\s*\{?", line)
        if decl_match:
            kind, name = decl_match.groups()
            if len(block_stack) == 0 and kind in {"message", "enum"}:
                type_names.append(name)
            if len(block_stack) == 0 and kind == "enum":
                current_enum = EnumSpec(name=name, values=[])
                enums.append(current_enum)
            block_stack.append(kind)
            open_count -= 1

        if current_enum and len(block_stack) == 1:
            value_match = re.match(r"([A-Z][A-Z0-9_]*)\s*=", line)
            if value_match:
                current_enum.values.append(value_match.group(1))

        for _ in range(close_count):
            if block_stack:
                finished = block_stack.pop()
                if finished == "enum" and len(block_stack) == 0:
                    current_enum = None

        for _ in range(max(open_count, 0)):
            block_stack.append("{")

    if not package:
        raise ValueError(f"Missing package declaration in {proto_path}")

    return ProtoSpec(package=package, type_names=type_names, enums=enums)


def is_enum_class(node: ast.ClassDef) -> bool:
    for base in node.bases:
        if isinstance(base, ast.Attribute) and base.attr == "Enum":
            return True
        if isinstance(base, ast.Name) and base.id == "Enum":
            return True
    return False


def collect_module_shapes(module_path: Path) -> tuple[dict[str, ast.ClassDef], dict[str, list[str]]]:
    tree = ast.parse(module_path.read_text(encoding="utf-8"))
    class_defs: dict[str, ast.ClassDef] = {}
    enum_members: dict[str, list[str]] = {}

    for node in tree.body:
        if not isinstance(node, ast.ClassDef):
            continue
        class_defs[node.name] = node
        if not is_enum_class(node):
            continue

        members: list[str] = []
        for item in node.body:
            if isinstance(item, ast.Assign):
                for target in item.targets:
                    if isinstance(target, ast.Name):
                        members.append(target.id)
        enum_members[node.name] = members

    return class_defs, enum_members


def common_token_prefix(values: list[str]) -> str:
    if len(values) < 2:
        return ""
    token_lists = [value.split("_") for value in values]
    prefix: list[str] = []
    for tokens in zip(*token_lists):
        if len(set(tokens)) != 1:
            break
        prefix.append(tokens[0])
    if not prefix:
        return ""
    return "_".join(prefix) + "_"


def build_alias_lines(proto_spec: ProtoSpec, module_path: Path) -> list[str]:
    class_defs, enum_members = collect_module_shapes(module_path)
    lines: list[str] = []

    for proto_name in proto_spec.type_names:
        actual_name = next(
            (name for name in class_defs if name.lower() == proto_name.lower()),
            None,
        )
        if actual_name and actual_name != proto_name:
            lines.append(f"{proto_name} = {actual_name}")

    for enum_spec in proto_spec.enums:
        actual_name = next(
            (name for name in enum_members if name.lower() == enum_spec.name.lower()),
            None,
        )
        if not actual_name:
            continue

        actual_members = set(enum_members[actual_name])
        prefix = common_token_prefix(enum_spec.values)
        for raw_value in enum_spec.values:
            if raw_value in actual_members:
                continue
            if prefix and raw_value.startswith(prefix):
                candidate = raw_value[len(prefix) :]
                if candidate in actual_members:
                    lines.append(
                        f'type.__setattr__({actual_name}.__class__, "{raw_value}", {actual_name}.{candidate})'
                    )
                    lines.append(
                        f'{actual_name}._member_map_["{raw_value}"] = {actual_name}.{candidate}'
                    )

    return lines


def write_alias_block(module_path: Path, alias_lines: list[str]) -> None:
    content = module_path.read_text(encoding="utf-8")
    compat_block = ""
    if alias_lines:
        compat_block = (
            f"\n{COMPAT_START}\n"
            "# Compatibility aliases for older BetterProto output.\n"
            + "\n".join(alias_lines)
            + f"\n{COMPAT_END}\n"
        )

    pattern = re.compile(
        rf"\n?{re.escape(COMPAT_START)}.*?{re.escape(COMPAT_END)}\n?",
        re.DOTALL,
    )
    content = re.sub(pattern, "\n", content).rstrip() + "\n"
    if compat_block:
        content = content.rstrip() + compat_block

    module_path.write_text(content, encoding="utf-8")


def main() -> int:
    repo_root = Path(__file__).resolve().parents[1]
    proto_dir = repo_root / "protos"
    out_root = repo_root / "python/proto-betterproto/src/zk_proto_betterproto"

    updated = 0
    for proto_path in sorted(proto_dir.glob("*.proto")):
        proto_spec = parse_proto(proto_path)
        module_path = out_root / proto_spec.package.replace(".", "/") / "__init__.py"
        if not module_path.exists():
            continue
        alias_lines = build_alias_lines(proto_spec, module_path)
        write_alias_block(module_path, alias_lines)
        updated += 1

    print(f"[OK] Applied legacy BetterProto compatibility aliases to {updated} modules")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
