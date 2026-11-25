#!/usr/bin/env python3
"""Generates crates/j8bytecode/wasm.json from V8's src/wasm/wasm-opcodes.h: j8's WebAssembly bytecode, a three-address
register form of the WebAssembly 3.0 instruction set.

Every WebAssembly 3.0 operator V8 lists keeps V8's name (`kExpr<Name>` without `kExpr`) and its V8 opcode (`wasm`,
the prefix shifted in as V8 does). Operators with a signature get one `Reg` per result and per parameter, then their
immediates; the rest are listed in `SHAPES` below. Structured control (`block` / `loop` / `if` / `else` / `end` /
`br` / `br_if`), `nop`, `drop`, the `local.*` operators and `try_table` disappear: they become `Jump*` / `Mov` and the
handler table. The bytecodes j8 adds have no `wasm` opcode.

Bytecodes are split into pages of at most 256: page 0 is one byte, page 1 (SIMD) follows the `Simd` page prefix.

WebAssembly 3.0 leaves out V8's threads (atomics), wide arithmetic, stringref, custom descriptors, stack switching
(WasmFX), FP16, legacy exception handling and testing operators; `EXCLUDED` drops them.

    tools/wasmbytecodes.py <v8 checkout> > crates/j8bytecode/wasm.json
"""
import json
import re
import sys

EXCLUDED_GROUPS = {'FOREACH_ATOMIC_OPCODE', 'FOREACH_ATOMIC_0_OPERAND_OPCODE', 'FOREACH_ATOMIC_GC_OPCODE',
                   'FOREACH_NUMERIC_OPCODE_WIDE', 'FOREACH_WASMFX_OPCODE'}
EXCLUDED = {'Try', 'Catch', 'Rethrow', 'Delegate', 'CatchAll', 'NopForTestingUnsupportedInLiftoff', 'RefI31Shared',
            'StructNewDesc', 'StructNewDefaultDesc', 'RefGetDesc', 'RefCastDescEq', 'RefCastDescEqNull',
            'BrOnCastDescEq', 'BrOnCastDescEqFail', 'RefCastNop'}
DISSOLVED = {'Nop', 'Block', 'Loop', 'If', 'Else', 'End', 'Br', 'BrIf', 'TryTable', 'Drop', 'SelectWithType',
             'LocalGet', 'LocalSet', 'LocalTee'}

OWN = [
    ('Wide', []), ('ExtraWide', []), ('Simd', []),
    ('Jump', ['Offset']), ('JumpIfZero', ['Reg', 'Offset']), ('JumpIfNonZero', ['Reg', 'Offset']),
    ('Mov', ['Reg', 'Reg']), ('I64ConstPool', ['Reg', 'Idx']),
]

SHAPES = {
    'Unreachable': [], 'Throw': ['Tag', 'Reg'], 'ThrowRef': ['Reg'], 'BrTable': ['Reg', 'Idx'], 'Return': ['Reg', 'Count'],
    'BrOnNull': ['Reg', 'Offset'], 'BrOnNonNull': ['Reg', 'Offset'],
    'CallFunction': ['Func', 'Reg'], 'CallIndirect': ['Type', 'Table', 'Reg', 'Reg'], 'CallRef': ['Type', 'Reg', 'Reg'],
    'ReturnCall': ['Func', 'Reg'], 'ReturnCallIndirect': ['Type', 'Table', 'Reg', 'Reg'], 'ReturnCallRef': ['Type', 'Reg', 'Reg'],
    'Select': ['Reg', 'Reg', 'Reg', 'Reg'],
    'GlobalGet': ['Reg', 'Global'], 'GlobalSet': ['Global', 'Reg'],
    'TableGet': ['Reg', 'Reg', 'Table'], 'TableSet': ['Reg', 'Reg', 'Table'],
    'I32Const': ['Reg', 'Imm'], 'I64Const': ['Reg', 'Imm'], 'F32Const': ['Reg', 'Imm'], 'F64Const': ['Reg', 'Idx'],
    'RefNull': ['Reg'], 'RefIsNull': ['Reg', 'Reg'], 'RefFunc': ['Reg', 'Func'], 'RefAsNonNull': ['Reg', 'Reg'],
    'RefEq': ['Reg', 'Reg', 'Reg'],
    'S128Const': ['Reg', 'Idx'],
    'MemoryInit': ['Reg', 'Memory', 'Data'], 'MemoryCopy': ['Reg', 'Memory', 'Memory'], 'MemoryFill': ['Reg', 'Memory'],
    'TableGrow': ['Reg', 'Reg', 'Reg', 'Table'], 'TableFill': ['Reg', 'Table'],
    'StructNew': ['Reg', 'Type', 'Reg'], 'StructNewDefault': ['Reg', 'Type'],
    'StructGet': ['Reg', 'Reg', 'Type', 'Field'], 'StructGetS': ['Reg', 'Reg', 'Type', 'Field'],
    'StructGetU': ['Reg', 'Reg', 'Type', 'Field'], 'StructSet': ['Reg', 'Reg', 'Type', 'Field'],
    'ArrayNew': ['Reg', 'Reg', 'Reg', 'Type'], 'ArrayNewDefault': ['Reg', 'Reg', 'Type'],
    'ArrayNewFixed': ['Reg', 'Reg', 'Type', 'Count'], 'ArrayNewData': ['Reg', 'Reg', 'Type', 'Data'],
    'ArrayNewElem': ['Reg', 'Reg', 'Type', 'Elem'],
    'ArrayGet': ['Reg', 'Reg', 'Reg', 'Type'], 'ArrayGetS': ['Reg', 'Reg', 'Reg', 'Type'], 'ArrayGetU': ['Reg', 'Reg', 'Reg', 'Type'],
    'ArraySet': ['Reg', 'Reg', 'Reg', 'Type'], 'ArrayLen': ['Reg', 'Reg'], 'ArrayFill': ['Reg', 'Type'],
    'ArrayCopy': ['Reg', 'Type', 'Type'], 'ArrayInitData': ['Reg', 'Type', 'Data'], 'ArrayInitElem': ['Reg', 'Type', 'Elem'],
    'RefTest': ['Reg', 'Reg', 'HeapType'], 'RefTestNull': ['Reg', 'Reg', 'HeapType'],
    'RefCast': ['Reg', 'Reg', 'HeapType'], 'RefCastNull': ['Reg', 'Reg', 'HeapType'],
    'BrOnCast': ['Reg', 'Flag8', 'HeapType', 'HeapType', 'Offset'], 'BrOnCastFail': ['Reg', 'Flag8', 'HeapType', 'HeapType', 'Offset'],
    'AnyConvertExtern': ['Reg', 'Reg'], 'ExternConvertAny': ['Reg', 'Reg'],
    'RefI31': ['Reg', 'Reg'], 'I31GetS': ['Reg', 'Reg'], 'I31GetU': ['Reg', 'Reg'],
}

IMMEDIATES = {
    'FOREACH_LOAD_MEM_OPCODE': ['Memory', 'MemOffset'], 'FOREACH_STORE_MEM_OPCODE': ['Memory', 'MemOffset'],
    'FOREACH_SIMD_MEM_OPCODE': ['Memory', 'MemOffset'], 'FOREACH_SIMD_MEM_1_OPERAND_OPCODE': ['Memory', 'MemOffset', 'Lane'],
    'FOREACH_MISC_MEM_OPCODE': ['Memory'], 'FOREACH_SIMD_MASK_OPERAND_OPCODE': ['Idx'],
}
NAMED_IMMEDIATES = {'DataDrop': ['Data'], 'TableInit': ['Table', 'Elem'], 'ElemDrop': ['Elem'],
                    'TableCopy': ['Table', 'Table'], 'TableSize': ['Table']}
LANES = {'FOREACH_SIMD_1_OPERAND_1_PARAM_OPCODE': ['Reg', 'Reg', 'Lane'],
         'FOREACH_SIMD_1_OPERAND_2_PARAM_OPCODE': ['Reg', 'Reg', 'Reg', 'Lane']}

OPERAND_TYPES = [('Reg', 'ScalableUnsignedByte'), ('Imm', 'ScalableSignedByte'), ('Offset', 'ScalableSignedByte'),
                 ('Count', 'ScalableUnsignedByte'), ('Idx', 'ScalableUnsignedByte'), ('Func', 'ScalableUnsignedByte'),
                 ('Type', 'ScalableUnsignedByte'), ('HeapType', 'ScalableSignedByte'), ('Field', 'ScalableUnsignedByte'),
                 ('Table', 'ScalableUnsignedByte'), ('Elem', 'ScalableUnsignedByte'), ('Memory', 'ScalableUnsignedByte'),
                 ('Data', 'ScalableUnsignedByte'), ('Global', 'ScalableUnsignedByte'), ('Tag', 'ScalableUnsignedByte'),
                 ('MemOffset', 'ScalableUnsignedByte'), ('Lane', 'FixedUnsignedByte'), ('Flag8', 'FixedUnsignedByte')]
PAGE = 256


def groups(text):
    """{FOREACH_X: [V(...) arguments or nested FOREACH_Y names, in order]}"""
    text = re.sub(r'/\*.*?\*/', ' ', text.replace('\\\n', ' '))
    out = {}
    for m in re.finditer(r'^#define[ \t]+(FOREACH_\w+)\(V\)(.*)$', text, re.M):
        items = []
        for it in re.finditer(r'(FOREACH_\w+)\(V\)|V\(([^()]*(?:\([^()]*\))?[^()]*)\)', m.group(2)):
            items.append(it.group(1) or [a.strip() for a in it.group(2).split(',')])
        out[m.group(1)] = items
    return out


def expand(gs, name, out):
    for it in gs[name]:
        if isinstance(it, str):
            if it not in EXCLUDED_GROUPS:
                expand(gs, it, out)
        else:
            out.append((name, it))
    return out


def operands(group, name, sig):
    if name in SHAPES:
        return SHAPES[name]
    if group in LANES:
        return LANES[group]
    if sig == '_':
        raise SystemExit(f'{name}: no signature and no shape')
    result, params = sig.split('_')
    regs = ['Reg'] * (len(result) if result != 'v' else 0) + ['Reg'] * (len(params) if params != 'v' else 0)
    return regs + IMMEDIATES.get(group, []) + NAMED_IMMEDIATES.get(name, [])


def main(v8):
    text = open(f'{v8}/src/wasm/wasm-opcodes.h').read()
    gs = groups(text)
    pages = [[{'name': n, 'operands': o} for n, o in OWN], []]
    for group, (name, opcode, sig, wat, *_) in expand(gs, 'FOREACH_OPCODE', []):
        if name in EXCLUDED or name in DISSOLVED or 'f16' in wat or wat.strip('"').startswith('string'):
            continue
        code = int(opcode, 16)
        page = 1 if group.startswith('FOREACH_SIMD') or group == 'FOREACH_RELAXED_SIMD_OPCODE' else 0
        pages[page].append({'name': name, 'wasm': code, 'operands': operands(group, name, sig)})
    pages[0].append({'name': 'Illegal', 'operands': []})
    for p in pages:
        assert len(p) <= PAGE, len(p)
    names = [b['name'] for p in pages for b in p]
    assert len(set(names)) == len(names)
    version = '.'.join(re.search(rf'#define V8_{k} (\d+)', open(f'{v8}/include/v8-version.h').read()).group(1)
                       for k in ('MAJOR_VERSION', 'MINOR_VERSION', 'BUILD_NUMBER'))
    json.dump({
        'source': f'V8 {version} src/wasm/wasm-opcodes.h, WebAssembly 3.0 (generated by tools/wasmbytecodes.py)',
        'operand_types': [{'name': n, 'info': i} for n, i in OPERAND_TYPES],
        'pages': [{'prefix': None, 'bytecodes': pages[0]}, {'prefix': 'Simd', 'bytecodes': pages[1]}],
    }, sys.stdout, indent=1, ensure_ascii=False)
    print(f'{len(pages[0])} + {len(pages[1])} bytecodes', file=sys.stderr)


if __name__ == '__main__':
    main(sys.argv[1])
