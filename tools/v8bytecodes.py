#!/usr/bin/env python3
"""Generates crates/j8bytecode/v8.json from V8's headers: the bytecode list (V8's numbering), every bytecode's implicit
accumulator use and operand types, the operand types with their size class, the runtime functions (`CallRuntime`'s
`RuntimeId`, V8's `Runtime::FunctionId` numbering), the interpreter intrinsics (`InvokeIntrinsic`'s `IntrinsicId`) and the
native context slots (`CallJSRuntime`'s `NativeContextIndex`, `Context::Field` without the `_INDEX` suffix).

The runtime function list depends on V8's build: this follows the x64 release build rusty_v8 ships (Intl, WebAssembly
and Sparkplug on; tracing, Dumpling and DrumBrake off).

    tools/v8bytecodes.py <v8 checkout> > crates/j8bytecode/v8.json
"""
import json
import re
import sys


def macros(text):
    """`#define NAME(args) body` (continuation lines joined) ⇒ {NAME: body}"""
    text = text.replace('\\\n', ' ')
    out = {}
    for m in re.finditer(r'^#define[ \t]+(\w+)\(([^)]*)\)[ \t]*(.*)$', text, re.M):
        out[m.group(1)] = m.group(3)
    return out


def calls(body):
    """Top-level `NAME(args)` calls in a macro body, in order"""
    body = re.sub(r'/\*.*?\*/', ' ', body)
    i, out = 0, []
    while i < len(body):
        m = re.compile(r'(\w+)\s*\(').match(body, i)
        if not m:
            i += 1
            continue
        depth, j = 1, m.end()
        while depth:
            depth += {'(': 1, ')': -1}.get(body[j], 0)
            j += 1
        out.append((m.group(1), body[m.end():j - 1]))
        i = j
    return out


def expand(defs, name, entries=('V', 'V_TSA')):
    """The entries (`V(...)` / `V_TSA(...)` by default) of list macro `name`, nested lists expanded"""
    out = []
    for callee, args in calls(defs[name]):
        if callee in entries:
            out.append([callee] + [a.strip() for a in args.split(',')])
        elif callee == 'IF_SPARKPLUG_PLUS' and FLAGS['V8_ENABLE_SPARKPLUG']:
            out.append([a.strip() for a in args.split(',')])
        elif callee in ('IF_WASM', 'IF_WASM_DRUMBRAKE'):
            flag = FLAGS['V8_ENABLE_WEBASSEMBLY'] if callee == 'IF_WASM' else FLAGS['V8_ENABLE_DRUMBRAKE']
            if flag:
                out.extend(expand(defs, args.split(',')[0].strip(), entries))
        elif callee in defs:
            out.extend(expand(defs, callee, entries))
    return out


# The build rusty_v8 ships (x64 release)
FLAGS = {'V8_INTL_SUPPORT': True, 'V8_ENABLE_WEBASSEMBLY': True, 'V8_ENABLE_SPARKPLUG': True, 'V8_ENABLE_DRUMBRAKE': False, 'V8_TRACE_UNOPTIMIZED': False, 'V8_TRACE_FEEDBACK_UPDATES': False, 'V8_DUMPLING': False}


def preprocess(text):
    """Keeps the lines `#ifdef` / `#ifndef` / `#if` / `#else` select under FLAGS"""
    out, stack = [], []
    for line in text.split('\n'):
        m = re.match(r'\s*#\s*(ifdef|ifndef|if|else|endif)\b\s*(.*)', line)
        if m:
            kind, cond = m.groups()
            if kind in ('ifdef', 'ifndef', 'if'):
                name = re.sub(r'defined\((\w+)\)', r'\1', cond).split()[0] if cond.strip() else ''
                value = FLAGS.get(name, False) != (kind == 'ifndef')
                stack.append([value, value])
            elif kind == 'else':
                stack[-1][0] = not stack[-1][1]
            else:
                stack.pop()
            continue
        if all(v for v, _ in stack):
            out.append(line)
    return '\n'.join(out)


def enum_values(text, decl):
    """Enumerators of the C++ enum `decl` (e.g. `enum class FunctionKind`), aliases (`kLast = …`) skipped"""
    body = text.split(decl, 1)[1].split('{', 1)[1].split('};', 1)[0]
    body = re.sub(r'//[^\n]*|/\*.*?\*/', '', body, flags=re.S)
    out = []
    for e in body.split(','):
        e = e.strip()
        if e and '=' not in e:
            out.append(e)
    return out


def slot_sizes(text):
    """`FeedbackMetadata::GetSlotSize`: kind ⇒ entries"""
    body = text.split('int FeedbackMetadata::GetSlotSize', 1)[1].split('UNREACHABLE();\n}', 1)[0]
    sizes, pending = {}, []
    for m in re.finditer(r'case FeedbackSlotKind::k(\w+):|return (\d+);', body):
        if m.group(1):
            pending.append(m.group(1))
        else:
            sizes.update((k, int(m.group(2))) for k in pending)
            pending = []
    return sizes


def main(root):
    b = macros(open(f'{root}/src/interpreter/bytecodes.h').read())
    o = macros(open(f'{root}/src/interpreter/bytecode-operands.h').read())
    version = dict(re.findall(r'#define V8_(MAJOR_VERSION|MINOR_VERSION|BUILD_NUMBER|PATCH_LEVEL)\s+(\d+)', open(f'{root}/include/v8-version.h').read()))
    operand_types = [{'name': n, 'info': i.split('::k')[1]} for _, n, i in expand(o, 'OPERAND_TYPE_LIST')]
    r = macros(preprocess(open(f'{root}/src/runtime/runtime.h').read()))
    listed = expand(r, 'FOR_EACH_INTRINSIC_IMPL', ('F', 'I'))
    # Runtime::FunctionId: every function (inline-able ones too) in order, then the inline versions `Inline<name>`
    runtime = [{'name': e[1], 'args': int(e[2]), 'results': int(e[3])} for e in listed]
    runtime += [{'name': 'Inline' + e[1], 'args': int(e[2]), 'results': int(e[3])} for e in listed if e[0] == 'I']
    intrinsics = [{'name': e[1], 'args': int(e[3])} for e in expand(macros(open(f'{root}/src/interpreter/interpreter-intrinsics.h').read()), 'INTRINSICS_LIST')]
    bytecodes = []
    for entry in expand(b, 'BYTECODE_LIST'):
        _, name, use, *ops = entry
        bytecodes.append({'name': name, 'use': use.split('::k')[1], 'operands': [op.split('::k')[1] for op in ops]})
    raw = [(n, v) for n, v in re.findall(r'^\s*k(\w+)\s*=\s*([^,\n]+),?\s*$', open(f'{root}/src/interpreter/bytecode-operands.h').read().split('enum class ImplicitRegisterUse')[1].split('};')[0], re.M)]
    bits = {}
    for n, v in raw:
        bits[n] = eval(re.sub(r'k(\w+)', lambda m: str(bits[m.group(1)]), v))
    uses = [{'name': n, 'bits': bits[n]} for n, _ in raw]
    contexts = expand(macros(preprocess(open(f'{root}/src/objects/contexts.h').read())), 'NATIVE_CONTEXT_FIELDS')
    native_context = [n.removesuffix('_INDEX') for n in ['SCOPE_INFO_INDEX', 'PREVIOUS_INDEX', 'EXTENSION_INDEX'] + [e[1] for e in contexts]]
    globals_h = open(f'{root}/src/common/globals.h').read()
    sizes = slot_sizes(open(f'{root}/src/objects/feedback-vector-inl.h').read())
    feedback = [{'name': k[1:], 'size': sizes.get(k[1:], 0)} for k in enum_values(open(f'{root}/src/objects/feedback-vector.h').read(), 'enum class FeedbackSlotKind')]
    kinds = [k[1:] for k in enum_values(open(f'{root}/src/objects/function-kind.h').read(), 'enum class FunctionKind')]
    scopes = enum_values(globals_h, 'enum ScopeType')
    modes = [k[1:] for k in enum_values(globals_h, 'enum class VariableMode')]
    json.dump({
        'source': f"V8 {version['MAJOR_VERSION']}.{version['MINOR_VERSION']}.{version['BUILD_NUMBER']} src/interpreter/bytecodes.h, bytecode-operands.h, interpreter-intrinsics.h, src/runtime/runtime.h, src/objects/contexts.h, function-kind.h, feedback-vector.h, src/common/globals.h (generated by tools/v8bytecodes.py)",
        'implicit_register_use': uses,
        'operand_types': operand_types,
        'bytecodes': bytecodes,
        'runtime': runtime,
        'intrinsics': intrinsics,
        'native_context': native_context,
        'function_kinds': kinds,
        'feedback_slot_kinds': feedback,
        'scope_types': scopes,
        'variable_modes': modes,
    }, sys.stdout, indent=1)
    sys.stdout.write('\n')


main(sys.argv[1])
