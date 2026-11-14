## class `RICHEDIT50W`

msftedit.dll load: private 1.21 → 1.29 MiB

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
| wrap on: toggle time | 16.25 ms (200 lines, 20 long) | · |
| undo depth after on/off toggle (30 edits) | 30 undos, back to loaded text: true, modify -1 | ✔ |
| redo depth | 30, text = before toggle: true | ✔ |
| replace all via ST_KEEPUNDO|ST_SELECTION: one undo | can undo 1, after 1 undo `a x a x a x`, can undo 0 | ✔ |
| WM_CHAR cyrillic | `привіт, ґанок ї є` | ✔ |
| WM_CHAR surrogate pair | ends with emoji: true | ✔ |
| enter inserts | `\u{43f}\u{440}\u{438}\u{432}\u{456}\u{442}, \u{491}\u{430}\u{43d}\u{43e}\u{43a} \u{457} \u{454}\u{1f600}` | · |
| no EM_EXLIMITTEXT: set 100k chars | 100000 chars kept | · |
| no EM_EXLIMITTEXT: paste 100k chars | 100000 chars kept | · |
| first control, private | 2.02 MiB at creation → 3.96 MiB now | · |
| 5 tabs × 1 MiB text (utf-8 size; 16384 lines) | private 3.9 → loaded 13.3 (+9.4) → 200 undoable edits each 19.4 (+6.1) → destroyed 6.4 | · |
| big: 1 MiB, 31775 short lines | set 91 ms · lines 31776 (29 ms) · wrap on 29 ms → 31776 visual · tom paragraph(end) 31776 in 2.1 ms, (mid) 15888 in 1.2 ms · EM_EXLINEFROMCHAR(end) 31774 in 0.0 ms · caret to end 0 ms · get text 7 ms (true) · private 7 → 14 → 11 → freed 7 MiB | · |
| big: 10 MiB, 317750 short lines | set 861 ms · lines 317751 (318 ms) · wrap on 310 ms → 317751 visual · tom paragraph(end) 317751 in 27.3 ms, (mid) 158876 in 11.9 ms · EM_EXLINEFROMCHAR(end) 317749 in 0.8 ms · caret to end 0 ms · get text 64 ms (true) · private 21 → 48 → 58 → freed 19 MiB | · |
| big: 50 MiB, 1588751 short lines | set 4299 ms · lines 1588752 (1707 ms) · wrap on 1653 ms → 1588752 visual · tom paragraph(end) 1588752 in 114.3 ms, (mid) 794376 in 53.4 ms · EM_EXLINEFROMCHAR(end) 1588750 in 1.8 ms · caret to end 0 ms · get text 343 ms (true) · private 60 → 206 → 254 → freed 59 MiB | · |
| big: 64 KiB, one line, no wrap | set 6 ms · lines 4 (0 ms) · wrap on 4 ms → 83 visual · tom paragraph(end) 1 in 0.1 ms, (mid) 1 in 0.0 ms · EM_EXLINEFROMCHAR(end) 766 in 3.3 ms · caret to end 0 ms · get text 0 ms (true) · private 7 → 7 → 7 → freed 6 MiB | · |
| big: 256 KiB, one line, no wrap | set 24 ms · lines 13 (0 ms) · wrap on 8 ms → 83 visual · tom paragraph(end) 1 in 0.2 ms, (mid) 1 in 0.1 ms · EM_EXLINEFROMCHAR(end) 3065 in 6.0 ms · caret to end 0 ms · get text 1 ms (true) · private 7 → 7 → 7 → freed 7 MiB | · |
| big: 1024 KiB, one line, no wrap | set 104 ms · lines 52 (0 ms) · wrap on 28 ms → 83 visual · tom paragraph(end) 1 in 0.7 ms, (mid) 1 in 0.2 ms · EM_EXLINEFROMCHAR(end) 12263 in 23.0 ms · caret to end 0 ms · get text 6 ms (true) · private 7 → 10 → 11 → freed 7 MiB | · |
| big: 1 MiB, one line, wrap before load | set 87 ms · lines 12264 (27 ms) · wrap on 25 ms → 12264 visual · tom paragraph(end) 1 in 0.7 ms, (mid) 1 in 0.3 ms · EM_EXLINEFROMCHAR(end) 12263 in 0.0 ms · caret to end 0 ms · get text 7 ms (true) · private 7 → 9 → 11 → freed 7 MiB | · |
| big: 10 MiB, one line, wrap before load | set 751 ms · lines 122641 (226 ms) · wrap on 244 ms → 122641 visual · tom paragraph(end) 1 in 12.3 ms, (mid) 1 in 2.4 ms · EM_EXLINEFROMCHAR(end) 122640 in 0.2 ms · caret to end 0 ms · get text 57 ms (true) · private 18 → 41 → 59 → freed 18 MiB | · |
| big: 50 MiB, one line, wrap before load | set 4101 ms · lines 613203 (1191 ms) · wrap on 1222 ms → 613203 visual · tom paragraph(end) 1 in 27.9 ms, (mid) 1 in 12.5 ms · EM_EXLINEFROMCHAR(end) 613202 in 1.6 ms · caret to end 0 ms · get text 349 ms (true) · private 64 → 176 → 266 → freed 64 MiB | · |

