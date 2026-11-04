//! Dev tool: decode a relay trace (tools/perf/delay_relay.py --trace) with the session key.
//! Usage: trace_decode <trace.txt> <base64-key>
//! Input lines: `<monotonic seconds> <C|S> <hex datagram>` (C = sent by the client, S = received
//! by the client). Output, one line per datagram, tab separated:
//! `t dir seq old new ack thr difflen databytes echo nfrag complete`
//! (`old..thr`, `echo` are `-` until the message completes on its last fragment).

use moche_core::crypto::Session;
use moche_core::network::{Cipher, Inner, Reassembler};
use moche_core::proto::{decode_diff, Element, Instruction};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap_or(0))
        .collect()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let session = Session::from_base64(&a[2]).expect("key");
    let cipher = Cipher::new(&session);
    let text = std::fs::read_to_string(&a[1]).expect("trace");
    let mut reasm = [Reassembler::new(), Reassembler::new()];
    for line in text.lines() {
        let mut p = line.split(' ');
        let (Some(t), Some(d), Some(h)) = (p.next(), p.next(), p.next()) else {
            continue;
        };
        let Ok(op) = cipher.open(&unhex(h)) else {
            println!("{t}\t{d}\t-\tUNDECRYPTABLE");
            continue;
        };
        let Ok(inner) = Inner::decode(&op.plaintext) else {
            println!("{t}\t{d}\t{}\tBADINNER", op.seq);
            continue;
        };
        let side = usize::from(d == "S");
        let nf = inner.frag.num + 1;
        match reasm[side].push(inner.frag) {
            Ok(Some(m)) => {
                let Ok(i) = Instruction::decode(&m) else {
                    println!("{t}\t{d}\t{}\tBADINSTR", op.seq);
                    continue;
                };
                let els = decode_diff(&i.diff).unwrap_or_default();
                let data: usize = els
                    .iter()
                    .map(|e| if let Element::Data(b) = e { b.len() } else { 0 })
                    .sum();
                let echo = els
                    .iter()
                    .find_map(|e| {
                        if let Element::EchoAck(n) = e {
                            Some(*n as i64)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(-1);
                println!(
                    "{t}\t{d}\t{}\t{}\t{}\t{}\t{}\t{}\t{data}\t{echo}\t{nf}\t1",
                    op.seq,
                    i.old_num,
                    i.new_num as i64,
                    i.ack_num,
                    i.throwaway_num,
                    i.diff.len()
                );
            }
            _ => println!("{t}\t{d}\t{}\t-\t-\t-\t-\t-\t-\t-\t{nf}\t0", op.seq),
        }
    }
}
