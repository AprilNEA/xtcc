# This script is embedded in LLDB only by the Rust bootstrap.
# Rust supplies pid, library, and expected_tccd as JSON literals.
# LLDB still relies on its own Python runtime; Rust does not replace its Python API.
import json
import os
import stat

import lldb

report = None
process = None
error = lldb.SBError()
try:
    if not isinstance(pid, int) or isinstance(pid, bool) or pid <= 0:
        raise RuntimeError("invalid pid")
    if expected_tccd != "/System/Library/PrivateFrameworks/TCC.framework/Support/tccd":
        raise RuntimeError("invalid target path")
    if not os.path.isabs(library) or not stat.S_ISREG(os.stat(library).st_mode):
        raise RuntimeError("library is not an absolute regular file")
    if os.path.realpath(library) != library:
        raise RuntimeError("library path changed")

    target = lldb.debugger.CreateTarget("")
    process = target.AttachToProcessWithID(lldb.debugger.GetListener(), pid, error)
    if error.Fail() or not process.IsValid():
        raise RuntimeError(str(error) or "attach failed")

    spec = target.GetExecutable()
    executable = os.path.join(spec.GetDirectory() or "", spec.GetFilename() or "")
    if os.path.realpath(executable) != expected_tccd:
        raise RuntimeError("target is not tccd")
    if process.GetProcessID() != pid:
        raise RuntimeError("target pid changed")

    loaded = False
    for index in range(target.GetNumModules()):
        module = target.GetModuleAtIndex(index).GetFileSpec()
        path = os.path.join(module.GetDirectory() or "", module.GetFilename() or "")
        if path and os.path.realpath(path) == library:
            loaded = True
    if not loaded:
        error.Clear()
        process.LoadImage(lldb.SBFileSpec(library), error)
        if error.Fail():
            raise RuntimeError(str(error))

    def address(name):
        symbols = target.FindSymbols(name, lldb.eSymbolTypeData)
        if symbols.GetSize() != 1:
            raise RuntimeError("missing or ambiguous symbol " + name)
        value = symbols.GetContextAtIndex(0).GetSymbol().GetStartAddress().GetLoadAddress(target)
        if value == lldb.LLDB_INVALID_ADDRESS:
            raise RuntimeError("invalid symbol address " + name)
        return value

    def integer(name):
        location = address(name)
        error.Clear()
        value = process.ReadUnsignedFromMemory(location, 4, error)
        if error.Fail():
            raise RuntimeError(str(error))
        return value

    ready = integer("cyan_tccd_inject_ready")
    receive = integer("cyan_tccd_control_receive")
    if ready != 1 or not receive or receive == 0xFFFFFFFF:
        raise RuntimeError("injection did not initialize")
    report = {"pid": pid, "receive": receive}
except Exception as failure:
    print("INJECT_ERROR " + str(failure))
finally:
    # Detach from any valid process even if path validation, loading, or memory reads fail.
    if process is not None and process.IsValid():
        detached = process.Detach()
        if detached.Fail():
            print("INJECT_ERROR detach: " + str(detached))
            report = None

if report is not None:
    print("INJECT_RESULT " + json.dumps(report, sort_keys=True))
