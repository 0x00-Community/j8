use j8bytecode::{Decoder, Encoder, OperandScale, OperandTypeInfo, OperandValue, Opcode, WasmBytecode};

#[test]
fn every_wasm_bytecode_round_trips() {
    for &b in &WasmBytecode::ALL {
        if b.wasm().is_some() {
            assert_eq!(b.wasm().and_then(WasmBytecode::from_wasm), Some(b));
        }
        if Opcode::prefix_scale(b).is_some() || b.page_prefix().is_some() {
            continue;
        }
        for (wide, scale) in [(1, OperandScale::Single), (300, OperandScale::Double), (70_000, OperandScale::Quadruple)] {
            let operands: Vec<OperandValue> = b.operand_infos().map(|t| if t.is_scalable() { wide } else if t == OperandTypeInfo::FixedUnsignedByte { 7 } else { 1 }).collect();
            let mut e = Encoder::default();
            e.emit(b, &operands).expect("arity");
            let d = Decoder::<WasmBytecode>::decode(&e.code, 0).expect("decodes");
            assert_eq!(d.bytecode, b);
            assert_eq!(&*d.operands, &operands[..]);
            assert_eq!(d.next, e.code.len());
            let scalable = b.operand_infos().any(OperandTypeInfo::is_scalable);
            assert_eq!(d.scale, if scalable { scale } else { OperandScale::Single }, "{b:?}");
        }
    }
}
