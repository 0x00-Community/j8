use j8bytecode::{Bytecode, Decoder, Intrinsic, Register, Runtime};

fn printed_register(arg: &str) -> Option<Register> {
    let first = arg.split('-').next()?;
    match first {
        "<this>" => Some(Register::receiver()),
        "<context>" => Some(Register::current_context()),
        "<closure>" => Some(Register::function_closure()),
        _ => match first.split_at_checked(1)? {
            ("r", n) => n.parse().ok().map(Register),
            ("a", n) => n.parse::<i32>().ok().map(|n| Register::parameter(n + 1)),
            _ => None,
        },
    }
}

#[test]
fn decodes_v8_output() {
    let mut registers = 0;
    for line in include_str!("v8-15.2.124.txt").lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let n = words.iter().take_while(|w| w.len() == 2 && u8::from_str_radix(w, 16).is_ok()).count();
        let bytes: Vec<u8> = words[..n].iter().map(|w| u8::from_str_radix(w, 16).expect("hex")).collect();
        let name = words[n].trim_end_matches(".ExtraWide").trim_end_matches(".Wide");
        let d = Decoder::decode(&bytes, 0).expect(line);
        assert_eq!(format!("{:?}", d.bytecode), name, "{line}");
        assert_eq!(d.next, bytes.len(), "{line}");
        let args = words[n + 1..].join(" ");
        let first = args.split(']').next().unwrap_or("").trim_start_matches('[');
        match d.bytecode {
            Bytecode::CallRuntime | Bytecode::CallRuntimeForPair => {
                let id = Runtime::from_u16(u16::try_from(d.operands[0]).expect("runtime id")).expect("runtime id");
                assert_eq!(format!("{id:?}"), first, "{line}");
            }
            Bytecode::InvokeIntrinsic => {
                let id = Intrinsic::from_u8(u8::try_from(d.operands[0]).expect("intrinsic id")).expect("intrinsic id");
                assert_eq!(format!("{id:?}"), first.trim_start_matches('_'), "{line}");
            }
            _ => {}
        }
        let printed = args.split(", ").filter_map(printed_register);
        let ours = d.bytecode.operand_types().iter().zip(d.operands.iter()).filter(|(t, _)| t.is_register()).map(|(_, &v)| Register::from(i32::try_from(v).expect("register operand")));
        for (p, o) in printed.zip(ours) {
            assert_eq!(p, o, "{line}");
            registers += 1;
        }
    }
    assert!(registers > 300);
}
