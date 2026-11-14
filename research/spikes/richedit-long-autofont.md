## class `RICHEDIT50W`

msftedit.dll load: private 1.22 → 1.30 MiB

| check | result | |
|---|---|---|
| fidelity: crlf `\u{43e}\u{434}\u{438}\u{43d}\r\n\u{434}\u{432}\u{430}\r\n` | raw `\u{43e}\u{434}\u{438}\u{43d}\r\u{434}\u{432}\u{430}\r` · crlf `\u{43e}\u{434}\u{438}\u{43d}\r\n\u{434}\u{432}\u{430}\r\n` | ✔ |
| fidelity: lf `\u{43e}\u{434}\u{438}\u{43d}\n\u{434}\u{432}\u{430}\n` | raw `\u{43e}\u{434}\u{438}\u{43d}\r\u{434}\u{432}\u{430}\r` · crlf `\u{43e}\u{434}\u{438}\u{43d}\r\n\u{434}\u{432}\u{430}\r\n` | ✘ |
| fidelity: lone cr `\u{43e}\u{434}\u{438}\u{43d}\r\u{434}\u{432}\u{430}` | raw `\u{43e}\u{434}\u{438}\u{43d}\r\u{434}\u{432}\u{430}` · crlf `\u{43e}\u{434}\u{438}\u{43d}\r\n\u{434}\u{432}\u{430}` | ✔ |
| fidelity: mixed `a\r\nb\nc\rd` | raw `a\rb\rc\rd` · crlf `a\r\nb\r\nc\r\nd` | ✘ |
| fidelity: no final newline `a\r\nb` | raw `a\rb` · crlf `a\r\nb` | ✔ |
| fidelity: vt / ff `a\vb\fc` | raw `a\vb\fc` · crlf `a\r\nb\fc` | ✔ |
| fidelity: u+2028 / u+2029 / u+0085 `a\u2028b\u2029c\u0085d` | raw `a\vb\rc\u0085d` · crlf `a\r\nb\r\nc\u0085d` | ✘ |
| fidelity: nul `a\0b` | raw `a` · crlf `a` | ✘ |
| fidelity: emoji + zwj `\u{1f600} \u{1f469}\u{200d}\u{1f469}\u{200d}\u{1f467} \u{1f1fa}\u{1f1e6}` | raw `\u{1f600} \u{1f469}\u{200d}\u{1f469}\u{200d}\u{1f467} \u{1f1fa}\u{1f1e6}` · crlf `\u{1f600} \u{1f469}\u{200d}\u{1f469}\u{200d}\u{1f467} \u{1f1fa}\u{1f1e6}` | ✔ |
| fidelity: combining `\u{439} (\u{438}\u{306}) e\u{301}` | raw `\u{439} (\u{438}\u{306}) e\u{301}` · crlf `\u{439} (\u{438}\u{306}) e\u{301}` | ✔ |
| fidelity: tab `a\tb` | raw `a\tb` · crlf `a\tb` | ✔ |
| fidelity: cr cr lf `a\r\r\nb` | raw `ab` · crlf `ab` | ✘ |
| units: len of `a😀b\r\nc` (utf-16 = 6) | 6 | · |
| units: caret 3 = after emoji | sel (3, 3), text 0..3 `a\u{1f600}` | ✔ |
| units: caret 2 = inside surrogate pair | snaps to (1, 1) | · |
| units: cp of `c` (source utf-16 6, cr-only 5) | 5 | · |
| wrap on: text identical | true | ✔ |
| wrap on: selection | (5000, 5100) → (5000, 5100) | ✔ |
| wrap on: modify flag | -1 → -1 | ✔ |
| wrap on: can undo | 1 → 1 | ✔ |
| wrap on: scroll pos (px) | (9972, 1120) → (9972, 6636) | · |
| wrap on: first visible char | 15012 → 15012 | · |
| wrap on: toggle time | 6.47 ms (200 lines, 20 long) | · |
| undo depth after on/off toggle (30 edits) | 30 undos, back to loaded text: true, modify -1 | ✔ |
| redo depth | 30, text = before toggle: true | ✔ |
| replace all via ST_KEEPUNDO|ST_SELECTION: one undo | can undo 1, after 1 undo `a x a x a x`, can undo 0 | ✔ |
| WM_CHAR cyrillic | `привіт, ґанок ї є` | ✔ |
| WM_CHAR surrogate pair | ends with emoji: true | ✔ |
| enter inserts | `\u{43f}\u{440}\u{438}\u{432}\u{456}\u{442}, \u{491}\u{430}\u{43d}\u{43e}\u{43a} \u{457} \u{454}\u{1f600}` | · |
| no EM_EXLIMITTEXT: set 100k chars | 100000 chars kept | · |
| no EM_EXLIMITTEXT: paste 100k chars | 100000 chars kept | · |
| first control, private | 2.00 MiB at creation → 6.84 MiB now | · |
| 5 tabs × 1 MiB text (utf-8 size; 16384 lines) | private 5.8 → loaded 22.4 (+16.6) → 200 undoable edits each 24.4 (+2.0) → destroyed 9.2 | · |
| big: 64 KiB, one line, no wrap | set 33 ms · lines 4 (0 ms) · wrap on 6 ms → 83 visual · tom paragraph(end) 1 in 0.1 ms, (mid) 1 in 0.0 ms · EM_EXLINEFROMCHAR(end) 766 in 4.7 ms · caret to end 0 ms · get text 0 ms (true) · private 9 → 8 → 9 → freed 8 MiB | · |
| big: 256 KiB, one line, no wrap | set 369 ms · lines 13 (0 ms) · wrap on 17 ms → 83 visual · tom paragraph(end) 1 in 0.2 ms, (mid) 1 in 0.1 ms · EM_EXLINEFROMCHAR(end) 3065 in 13.3 ms · caret to end 0 ms · get text 1 ms (true) · private 9 → 9 → 12 → freed 9 MiB | · |
| big: 1024 KiB, one line, no wrap | set 5255 ms · lines 52 (0 ms) · wrap on 58 ms → 83 visual · tom paragraph(end) 1 in 1.0 ms, (mid) 1 in 0.6 ms · EM_EXLINEFROMCHAR(end) 12263 in 57.7 ms · caret to end 0 ms · get text 5 ms (true) · private 10 → 17 → 20 → freed 10 MiB | · |
| big: 1 MiB, one line, wrap before load | set 5222 ms · lines 12264 (62 ms) · wrap on 51 ms → 12264 visual · tom paragraph(end) 1 in 0.8 ms, (mid) 1 in 0.5 ms · EM_EXLINEFROMCHAR(end) 12263 in 0.0 ms · caret to end 0 ms · get text 6 ms (true) · private 10 → 17 → 14 → freed 10 MiB | · |
