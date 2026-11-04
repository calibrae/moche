//! Wire neutrality of predictive echo: a transport driven next to an active
//! `Predictor` emits exactly the same datagrams, at the same times, with the same
//! deadlines, as an identical transport driven alone. (The types already enforce
//! it: the predictor takes `&Framebuffer` and byte counts, never a `Transport`.
//! `tools/perf/golden-diff.sh` proves it for the library as a whole.)

use moche_core::crypto::Session;
use moche_core::network::{fragment, Cipher, Direction, Inner};
use moche_core::predict::{PredictMode, Predictor};
use moche_core::proto::{encode_diff, Element, Instruction};
use moche_core::screen::Screen;
use moche_core::ssp::{Event, Transport};

const KEY: &str = "Ixu2un/jwqLpFzsnDHm5lw";

fn transport() -> Transport {
    let mut s = 1u64;
    Transport::new(
        &Session::from_base64(KEY).unwrap(),
        0,
        Box::new(move |b| {
            for x in b {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                *x = (s >> 40) as u8;
            }
        }),
    )
}

#[test]
fn predictor_next_to_a_transport_changes_no_datagram() {
    let server = Cipher::new(&Session::from_base64(KEY).unwrap());
    let mut plain = transport();
    let mut with = transport();
    let mut pred = Predictor::new(PredictMode::Always);
    let mut screen = Screen::new(80, 24);
    let (mut seq, mut id, mut state) = (0u64, 1u64, 0u64);
    let mut now = 0u64;
    let mut sent = 0usize;
    for step in 0..400u64 {
        now += 7;
        if step % 3 == 0 {
            let k = [b'a' + (step % 26) as u8];
            plain.send_keys(now, &k);
            with.send_keys(now, &k);
            pred.on_input(
                now,
                &k,
                screen.latest(),
                with.srtt_ms(),
                with.input_bytes_total(),
            );
        }
        if step % 5 == 0 {
            state += 1;
            let ins = Instruction {
                version: 2,
                old_num: state - 1,
                new_num: state,
                ack_num: 0,
                throwaway_num: state - 1,
                diff: encode_diff(&[
                    Element::Data(vec![b'a' + (step % 26) as u8]),
                    Element::EchoAck(state.min(3)),
                ]),
                chaff: vec![],
            };
            let datagrams: Vec<Vec<u8>> = fragment(id, &ins.encode())
                .unwrap()
                .into_iter()
                .map(|f| {
                    seq += 1;
                    let inner = Inner {
                        ts: now as u16,
                        ts_reply: None,
                        frag: f,
                    };
                    server
                        .seal(Direction::ToClient, seq, &inner.encode())
                        .unwrap()
                })
                .collect();
            id += 1;
            for d in &datagrams {
                let a = plain.handle_datagram(now, d);
                let b = with.handle_datagram(now, d);
                assert_eq!(a, b);
                for ev in b {
                    if let Event::HostDiff {
                        old_num,
                        new_num,
                        bytes,
                    } = ev
                    {
                        screen.apply(old_num, new_num, None, &bytes).unwrap();
                    }
                }
                pred.on_server_state(screen.latest(), with.echoed_input_bytes(), now);
            }
        }
        pred.tick(now, with.srtt_ms(), 0);
        assert_eq!(plain.next_deadline_ms(), with.next_deadline_ms());
        loop {
            let (a, b) = (plain.poll_transmit(now), with.poll_transmit(now));
            assert_eq!(a, b, "datagram differs at step {step}");
            if a.is_none() {
                break;
            }
            sent += 1;
        }
    }
    assert!(sent > 10, "the scenario sent {sent} datagrams");
    assert_eq!(plain.queued_bytes(), with.queued_bytes());
}
