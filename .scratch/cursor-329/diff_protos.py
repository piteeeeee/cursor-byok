#!/usr/bin/env python3
"""Structural diff between protocols/cursor/*.proto and .scratch/cursor-329/extracted-329/*.proto."""
import re
import os
import sys

def parse_proto(path):
    messages = {}   # name -> list of normalized field/oneof lines
    enums = {}      # name -> list of value lines
    services = {}   # name -> list of rpc lines
    if not os.path.exists(path):
        return messages, enums, services

    with open(path, "r", encoding="utf-8") as f:
        lines = f.readlines()

    stack = []  # ("message"|"enum"|"service"|"oneof", name)
    for raw in lines:
        line = raw.strip()
        if not line or line.startswith("//"):
            continue
        # normalize comment var names if inline
        m_msg = re.match(r"^message\s+([A-Za-z0-9_]+)\s*\{$", line)
        m_enum = re.match(r"^enum\s+([A-Za-z0-9_]+)\s*\{$", line)
        m_svc = re.match(r"^service\s+([A-Za-z0-9_]+)\s*\{$", line)
        m_oneof = re.match(r"^oneof\s+([A-Za-z0-9_]+)\s*\{$", line)

        if m_msg:
            name = m_msg.group(1)
            full = ".".join([s[1] for s in stack if s[0] == "message"] + [name])
            stack.append(("message", name, full))
            messages.setdefault(full, [])
        elif m_enum:
            name = m_enum.group(1)
            full = ".".join([s[1] for s in stack if s[0] == "message"] + [name])
            stack.append(("enum", name, full))
            enums.setdefault(full, [])
        elif m_svc:
            name = m_svc.group(1)
            stack.append(("service", name, name))
            services.setdefault(name, [])
        elif m_oneof:
            name = m_oneof.group(1)
            full = stack[-1][2] if stack else ""
            stack.append(("oneof", name, full))
            if full:
                messages[full].append(f"oneof {name}")
        elif line == "}":
            if stack:
                stack.pop()
        else:
            if not stack:
                continue
            kind, _, full = stack[-1]
            if kind == "message":
                messages[full].append(line)
            elif kind == "oneof":
                messages[full].append(f"  {line}")
            elif kind == "enum":
                enums[full].append(line)
            elif kind == "service":
                services[full].append(line)

    return messages, enums, services

def diff_file(old_path, new_path, name):
    om, oe, os_ = parse_proto(old_path)
    nm, ne, ns = parse_proto(new_path)

    print(f"\n================ {name} ================")
    print(f"Messages: {len(om)} -> {len(nm)} | Enums: {len(oe)} -> {len(ne)} | Services: {len(os_)} -> {len(ns)}")

    # Services diff
    for svc in sorted(set(os_) | set(ns)):
        if svc not in os_:
            print(f"  [SVC +] {svc} ({len(ns[svc])} rpcs)")
            for r in ns[svc]:
                print(f"      + {r}")
        elif svc not in ns:
            print(f"  [SVC -] {svc}")
        else:
            o_set = set(os_[svc])
            n_set = set(ns[svc])
            if o_set != n_set:
                print(f"  [SVC ~] {svc}")
                for r in sorted(o_set - n_set):
                    print(f"      - {r}")
                for r in sorted(n_set - o_set):
                    print(f"      + {r}")

    # Changed messages
    added_m = sorted(set(nm) - set(om))
    removed_m = sorted(set(om) - set(nm))
    changed_m = []
    for k in sorted(set(om) & set(nm)):
        if om[k] != nm[k]:
            changed_m.append(k)
    print(f"  Messages: +{len(added_m)}, -{len(removed_m)}, ~{len(changed_m)}")
    for k in changed_m:
        o_lines = set(om[k])
        n_lines = set(nm[k])
        if o_lines == n_lines:
            continue
        print(f"  [MSG ~] {k}")
        for l in sorted(o_lines - n_lines):
            print(f"      - {l}")
        for l in sorted(n_lines - o_lines):
            print(f"      + {l}")

    added_e = sorted(set(ne) - set(oe))
    removed_e = sorted(set(oe) - set(ne))
    changed_e = [k for k in sorted(set(oe) & set(ne)) if set(oe[k]) != set(ne[k])]
    print(f"  Enums: +{len(added_e)}, -{len(removed_e)}, ~{len(changed_e)}")
    for k in changed_e:
        print(f"  [ENUM ~] {k}")
        for l in sorted(set(oe[k]) - set(ne[k])):
            print(f"      - {l}")
        for l in sorted(set(ne[k]) - set(oe[k])):
            print(f"      + {l}")

    return added_m, removed_m, added_e, removed_e

if __name__ == "__main__":
    old_dir = "protocols/cursor"
    new_dir = ".scratch/cursor-329/extracted-329"
    files = sorted(set(os.listdir(old_dir)) | set(os.listdir(new_dir)))
    for fn in files:
        if not fn.endswith(".proto"):
            continue
        diff_file(os.path.join(old_dir, fn), os.path.join(new_dir, fn), fn)
