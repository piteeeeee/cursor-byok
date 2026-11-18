#!/usr/bin/env python3
"""Deep takeover-focused diff between protocols/cursor/*.proto and .scratch/cursor-329/extracted-329/*.proto,
cross-referenced against server/src/ Rust usages."""
import os
import re
import glob

from diff_protos import parse_proto

OLD_DIR = "protocols/cursor"
NEW_DIR = ".scratch/cursor-329/extracted-329"

def load_rust_sources():
    chunks = []
    for p in glob.glob("server/src/**/*.rs", recursive=True) + glob.glob("server/tests/**/*.rs", recursive=True):
        with open(p, "r", encoding="utf-8") as f:
            chunks.append((p, f.read()))
    return chunks

RUST_FILES = load_rust_sources()
ALL_RUST = "\n".join(c for _, c in RUST_FILES)

def camel_to_snake(name):
    s1 = re.sub("(.)([A-Z][a-z]+)", r"\1_\2", name)
    return re.sub("([a-z0-9])([A-Z])", r"\1_\2", s1).lower()

def snake_to_pascal(name):
    return "".join(part.capitalize() for part in name.split("_"))

print("=== 1. FILE-LEVEL SUMMARY ===")
all_files = sorted(set(os.listdir(OLD_DIR)) | set(os.listdir(NEW_DIR)))
for fn in all_files:
    if not fn.endswith(".proto"):
        continue
    op = os.path.join(OLD_DIR, fn)
    np = os.path.join(NEW_DIR, fn)
    om, oe, os_ = parse_proto(op)
    nm, ne, ns = parse_proto(np)
    print(f"{fn:22s}: msgs {len(om):4d} -> {len(nm):4d} (+{len(set(nm)-set(om)):3d}/-{len(set(om)-set(nm)):3d}) | "
          f"enums {len(oe):3d} -> {len(ne):3d} (+{len(set(ne)-set(oe)):2d}/-{len(set(oe)-set(ne)):2d}) | "
          f"svcs {len(os_):2d} -> {len(ns):2d} (+{len(set(ns)-set(os_)):2d}/-{len(set(os_)-set(ns)):2d})")

om_ag, oe_ag, os_ag = parse_proto(os.path.join(OLD_DIR, "agent_v1.proto"))
nm_ag, ne_ag, ns_ag = parse_proto(os.path.join(NEW_DIR, "agent_v1.proto"))
om_ai, oe_ai, os_ai = parse_proto(os.path.join(OLD_DIR, "aiserver_v1.proto"))
nm_ai, ne_ai, ns_ai = parse_proto(os.path.join(NEW_DIR, "aiserver_v1.proto"))

print("\n=== 2. TAKEOVER SERVICES DIFF (agent_v1 & aiserver_v1) ===")
WATCH_SVCS = [
    "AgentService", "BidiService", "AiService", "DashboardService",
    "ServerConfigService", "AuthService", "NetworkService", "AnalyticsService",
    "CppService", "FileSyncService", "ChatService", "InferenceService",
]
for label, os_, ns in [("agent.v1", os_ag, ns_ag), ("aiserver.v1", os_ai, ns_ai)]:
    for svc in sorted(set(os_) | set(ns)):
        if svc in WATCH_SVCS or ("Bidi" in svc) or ("Agent" in svc) or ("Run" in svc):
            if svc not in os_:
                print(f"  [{label} SVC +] {svc}: {len(ns[svc])} rpcs")
                for r in ns[svc]:
                    print(f"    + {r}")
            elif svc not in ns:
                print(f"  [{label} SVC -] {svc} (had {len(os_[svc])} rpcs)")
            else:
                o_set, n_set = set(os_[svc]), set(ns_[svc] if False else ns[svc])
                if o_set == n_set:
                    print(f"  [{label} SVC =] {svc} (unchanged, {len(o_set)} rpcs)")
                else:
                    print(f"  [{label} SVC ~] {svc}")
                    for r in sorted(o_set - n_set):
                        print(f"    - {r}")
                    for r in sorted(n_set - o_set):
                        print(f"    + {r}")

print("\n=== 3. CORE WIRE MESSAGES DIFF ===")
CORE_AI_MSGS = [
    "BidiAppendRequest", "BidiAppendResponse", "BidiPollRequest", "BidiPollResponse", "BidiRequestId",
    "ErrorDetails", "CustomErrorDetails", "ServerConfigResponse", "GetServerConfigResponse",
    "IsConnectedRequest", "IsConnectedResponse", "BootstrapStatsigRequest", "BootstrapStatsigResponse",
    "AvailableModelsRequest", "AvailableModelsResponse", "AvailableModelsResponse.AvailableModel",
    "WriteGitCommitMessageRequest", "WriteGitCommitMessageResponse",
]
for m in CORE_AI_MSGS:
    o = set(om_ai.get(m, []))
    n = set(nm_ai.get(m, []))
    if not o and not n:
        continue
    if o == n:
        print(f"  [aiserver.v1 =] {m} (unchanged, {len(o)} lines)")
    else:
        print(f"  [aiserver.v1 ~] {m}")
        for l in sorted(o - n):
            print(f"    - {l}")
        for l in sorted(n - o):
            print(f"    + {l}")

CORE_AGENT_MSGS = [
    "BidiRequestId",
    "AgentClientMessage", "AgentServerMessage",
    "AgentRunRequest", "PrewarmRequest", "ClientHeartbeat",
    "ConversationAction", "UserMessageAction", "UserMessage",
    "CancelAction", "ResumeAction", "SummarizeAction", "ExecutePlanAction",
    "BackgroundTaskCompletionAction", "AsyncAskQuestionCompletionAction",
    "InteractionUpdate", "TextDeltaUpdate", "ThinkingDeltaUpdate", "ThinkingCompletedUpdate",
    "PartialToolCallUpdate", "ToolCallStartedUpdate", "ToolCallCompletedUpdate", "ToolCallDeltaUpdate",
    "TurnEndedUpdate", "TokenDeltaUpdate", "HeartbeatUpdate", "UserMessageAppendedUpdate",
    "SummaryStartedUpdate", "SummaryUpdate", "SummaryCompletedUpdate", "ContextInjectionStateUpdate",
    "InteractionQuery", "InteractionResponse",
    "ExecServerMessage", "ExecClientMessage", "ExecServerControlMessage", "ExecClientControlMessage",
    "KvServerMessage", "KvClientMessage",
    "ConversationStateStructure", "AgentConversationTurnStructure",
    "RequestedModel", "ModelDetails", "RequestContext",
    "GetUsableModelsRequest", "GetUsableModelsResponse",
    "GetDefaultModelForCliRequest", "GetDefaultModelForCliResponse",
    "UpdateConversationMetadataRequest", "UpdateConversationMetadataResponse",
]
for m in CORE_AGENT_MSGS:
    o = set(om_ag.get(m, []))
    n = set(nm_ag.get(m, []))
    if not o and not n:
        continue
    if o == n:
        print(f"  [agent.v1 =] {m} (unchanged, {len(o)} lines)")
    else:
        print(f"  [agent.v1 ~] {m}")
        for l in sorted(o - n):
            print(f"    - {l}")
        for l in sorted(n - o):
            print(f"    + {l}")

print("\n=== 4. CHECK ANY REMOVED/CHANGED agent.v1 ITEMS REFERENCED IN server/ ===")
# Removed messages in agent_v1
for m in sorted(set(om_ag) - set(nm_ag)):
    short = m.split(".")[-1]
    if re.search(r"\b" + re.escape(short) + r"\b", ALL_RUST):
        print(f"  [REMOVED MSG USED IN RUST!] {m}")

# Removed fields in agent_v1 messages
field_re = re.compile(r"([A-Za-z0-9_]+)\s*=\s*(\d+)\s*;")
for m in sorted(set(om_ag) & set(nm_ag)):
    o_lines = set(om_ag[m]) - set(nm_ag[m])
    n_lines = set(nm_ag[m]) - set(om_ag[m])
    for l in sorted(o_lines):
        fm = field_re.search(l)
        if not fm:
            continue
        fname, ftag = fm.group(1), fm.group(2)
        pascal = snake_to_pascal(fname)
        # Check if fname or pascal appears in Rust
        used_snake = bool(re.search(r"\b" + re.escape(fname) + r"\b", ALL_RUST))
        used_pascal = bool(re.search(r"\b" + re.escape(pascal) + r"\b", ALL_RUST))
        if used_snake or used_pascal:
            print(f"  [CHANGED/REMOVED FIELD IN RUST?] {m}: -{l.strip()} (snake={used_snake}, pascal={used_pascal})")
            for nl in sorted(n_lines):
                if f"= {ftag};" in nl or fname in nl:
                    print(f"      new line: +{nl.strip()}")

print("\n=== 5. SEARCH FOR ANY 'data_binary' OR 'binary' FIELDS ADDED ACROSS ALL PROTOS ===")
for fn in ["agent_v1.proto", "aiserver_v1.proto"]:
    om, _, _ = parse_proto(os.path.join(OLD_DIR, fn))
    nm, _, _ = parse_proto(os.path.join(NEW_DIR, fn))
    for m in sorted(nm):
        o_set = set(om.get(m, []))
        n_set = set(nm[m])
        for l in sorted(n_set - o_set):
            if "binary" in l.lower() or ("bytes " in l and ("Bidi" in m or "Stream" in m or "Run" in m or "Poll" in m)):
                print(f"  [{fn} {m}] +{l.strip()}")
