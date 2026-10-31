#[path = "../../../tests/support/pty.rs"]
mod pty;

use sk_core::color::{delta_e_ok, rgb_to_xterm256, xterm256_color, Rgb8};
use sk_core::palette::{compute_frame, ModelConfig, Palette};
use sk_core::solar::Coordinates;
use sk_core::time::UtcInstant;
use sk_deliver::osc::{
    contains_palette_mutating_osc, encode_osc_frame, encode_osc_reset, format_osc_rgb, OscBackend,
    ST,
};
use sk_deliver::Backend;
use std::io::{self, Write};

fn escape_ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|&b| std::ascii::escape_default(b))
        .map(char::from)
        .collect()
}

fn format_osc_lines(bytes: &[u8]) -> String {
    let text = std::str::from_utf8(bytes).expect("OSC bytes must be valid ASCII/UTF-8");
    let parts: Vec<&str> = text.split_inclusive(ST).collect();
    parts
        .into_iter()
        .map(|part| escape_ascii(part.as_bytes()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Default)]
struct RecordingWriter {
    writes: Vec<Vec<u8>>,
    flushes: usize,
}

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writes.push(buf.to_vec());
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn osc_frame_matches_golden_snapshot_and_xterm_ctlseqs_format() {
    let palette = Palette::bundled_shiki();
    let model = ModelConfig::bundled();
    let tokyo = Coordinates::new(35.7, 139.7).unwrap();
    let instant = UtcInstant::parse_rfc3339("2026-03-20T20:35:00Z").unwrap();
    let frame = compute_frame(instant, tokyo, &palette, &model);

    let encoded = encode_osc_frame(&frame.colors);
    assert!(contains_palette_mutating_osc(&encoded));

    let text = std::str::from_utf8(&encoded).expect("valid ASCII");
    let seqs: Vec<&str> = text.split_inclusive(ST).collect();
    assert_eq!(
        seqs.len(),
        19,
        "expected bg (11), fg (10), cursor (12), and 16 ANSI (4;0..15) sequences"
    );

    assert_eq!(
        seqs[0],
        format!("\x1b]11;{}\x1b\\", format_osc_rgb(frame.colors.bg))
    );
    assert_eq!(
        seqs[1],
        format!("\x1b]10;{}\x1b\\", format_osc_rgb(frame.colors.fg))
    );
    assert_eq!(
        seqs[2],
        format!("\x1b]12;{}\x1b\\", format_osc_rgb(frame.colors.cursor))
    );
    for idx in 0..16 {
        assert_eq!(
            seqs[3 + idx],
            format!(
                "\x1b]4;{idx};{}\x1b\\",
                format_osc_rgb(frame.colors.ansi[idx])
            )
        );
    }

    let snapshot = format!(
        "byte_len = {}\nsequence_count = {}\n{}",
        encoded.len(),
        seqs.len(),
        format_osc_lines(&encoded)
    );
    insta::assert_snapshot!("osc_frame_shiki_spring_dawn", snapshot);
}

#[test]
fn osc_reset_emits_104_110_111_112_and_matches_golden_snapshot() {
    let reset = encode_osc_reset();
    assert_eq!(
        reset,
        b"\x1b]104\x1b\\\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\"
    );
    assert!(contains_palette_mutating_osc(&reset));

    let snapshot = format!("byte_len = {}\n{}", reset.len(), format_osc_lines(&reset));
    insta::assert_snapshot!("osc_reset_bytes", snapshot);
}

#[test]
fn sgr_sequences_are_not_flagged_as_palette_mutating_osc() {
    let sgr = b"\x1b[48;2;48;67;85m\x1b[38;2;245;248;252m sample \x1b[0m\n";
    assert!(!contains_palette_mutating_osc(sgr));
    let sgr_256 = b"\x1b[48;5;236m\x1b[38;5;255m sample \x1b[0m\n";
    assert!(!contains_palette_mutating_osc(sgr_256));
    assert!(!contains_palette_mutating_osc(b"plain NO_COLOR output\n"));
}

#[test]
fn osc_backend_writes_entire_frame_and_reset_atomically_in_single_write() {
    let palette = Palette::bundled_shiki();
    let model = ModelConfig::bundled();
    let coords = Coordinates::new(35.7, 139.7).unwrap();
    let frame = compute_frame(
        UtcInstant::parse_rfc3339("2026-06-22T02:45:00Z").unwrap(),
        coords,
        &palette,
        &model,
    );

    let mut backend = OscBackend::new(RecordingWriter::default());
    assert_eq!(backend.name(), "osc");
    backend.deliver(&frame).expect("deliver OSC frame");
    assert_eq!(
        backend.writer().writes.len(),
        1,
        "entire OSC frame must be delivered in a single write call"
    );
    assert_eq!(backend.writer().writes[0], encode_osc_frame(&frame.colors));
    assert_eq!(backend.writer().flushes, 1);

    backend.reset().expect("deliver OSC reset");
    assert_eq!(backend.writer().writes.len(), 2);
    assert_eq!(backend.writer().writes[1], encode_osc_reset());
    assert_eq!(backend.writer().flushes, 2);

    #[cfg(unix)]
    {
        use std::time::Duration;
        let pair = pty::PtyPair::open_raw().expect("open PTY pair");
        let expected = encode_osc_frame(&frame.colors);
        pair.write_to_replica(&expected)
            .expect("write OSC frame to PTY replica");
        let received = pair
            .read_exact_with_timeout(expected.len(), Duration::from_secs(5))
            .expect("read exact OSC frame from PTY controller");
        assert_eq!(received, expected);
    }
}

#[test]
fn xterm256_quantizer_maps_to_fixed_palette_16_to_255_minimizing_delta_e() {
    assert_eq!(rgb_to_xterm256(Rgb8::new(0, 0, 0)), 16);
    assert_eq!(xterm256_color(16), Rgb8::new(0, 0, 0));
    assert_eq!(rgb_to_xterm256(Rgb8::new(255, 255, 255)), 231);
    assert_eq!(xterm256_color(231), Rgb8::new(255, 255, 255));
    assert_eq!(rgb_to_xterm256(Rgb8::new(128, 128, 128)), 244);
    assert_eq!(xterm256_color(244), Rgb8::new(128, 128, 128));

    for idx in 16..=255_u8 {
        let exact = xterm256_color(idx);
        let mapped = rgb_to_xterm256(exact);
        assert!(
            mapped >= 16,
            "must never return mutable ANSI 0..=15 index: got {mapped}"
        );
        assert_eq!(
            xterm256_color(mapped),
            exact,
            "exact xterm-256 colour at index {idx} ({exact}) mapped to different colour {} ({})",
            mapped,
            xterm256_color(mapped)
        );
    }

    let sample = Rgb8::from_hex("#304355").unwrap();
    let best_idx = rgb_to_xterm256(sample);
    assert!(best_idx >= 16);
    let best_de = delta_e_ok(sample, xterm256_color(best_idx));
    for candidate in 16..=255_u8 {
        let de = delta_e_ok(sample, xterm256_color(candidate));
        assert!(
            best_de <= de + 1e-12,
            "index {candidate} had smaller delta_e ({de}) than chosen {best_idx} ({best_de})"
        );
    }
}
