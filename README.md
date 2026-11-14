# ryad

a small windows editor that keeps your drafts between launches.

press `ctrl+n`, write, close the window. open ryad again and the text is there, with the caret where you left it. open a log from explorer, press `alt+z` to wrap it, find what you need, copy it, save it.

**version 0.1.0**. windows 10/11, x64. free and open source (MIT). by lezneve.

## what it does

- **drafts without files.** a new tab needs no name and no save. everything you type is kept in ryad's own folder within a second of pausing (at least every 5 s while you keep typing), and comes back after you close ryad. if ryad is ended by force, the last kept copy comes back; what was typed after it may be lost.
- **your files change only when you say so.** edits to an open file come back as a local copy after closing; the file on disk stays as it was until `ctrl+s`. if another program changed the file meanwhile, ryad asks before overwriting.
- **plain text, kept as it was.** utf-8 with or without bom, utf-16 le/be with bom, windows-1251 on request; crlf, lf or cr and the final line break are written back the way they came.
- **tabs** for files and drafts side by side, **find and replace**, **go to line**, **wrap** per tab.
- **markdown preview** at the right of `.md` files: headings, lists, task lists, quotes, code blocks and tables, in your light or dark theme, with links and quote bars in the windows accent colour. `ctrl+shift+m` turns it on or off for any tab; drag the bar between text and preview to resize.

## keys

| keys | does |
|---|---|
| `ctrl+n` | new draft |
| `ctrl+o` | open files (several at once) |
| `ctrl+s` | save; a draft asks for a name |
| `ctrl+shift+s` | save as; the tab moves to the new file |
| `ctrl+w` | close the tab |
| `ctrl+tab` / `ctrl+shift+tab` | next / previous tab |
| `ctrl+shift+t` | bring back the last closed tab |
| `alt+z` | wrap lines in this tab (display only; the text is not changed) |
| `ctrl+shift+m` | markdown preview at the right, for this tab |
| `ctrl+f`, `f3`, `shift+f3` | find, next, previous |
| `ctrl+h` | replace one or all; "replace all" is undone in one step |
| `ctrl+g` | go to line |
| `ctrl+z` / `ctrl+y` | undo / redo, per tab |
| `esc` | close the find bar, back to the text |
| `alt+f4` / × | save the session and quit |

the same commands are in the menu (`alt`, `f10` or `alt+f/e/v/t` open it from the keyboard). a dot on a tab means changes that are not in the file yet.

## closing a changed tab

ryad asks: **save**, **keep as draft** (the tab closes; file › drafts brings it back, also after a restart), **discard changes** (asks once more; then the text is gone from ryad for good) or **cancel**.

## files that changed or disappeared

- changed on disk by another program: `ctrl+s` asks whether to save a copy, reload the file (your edits stay in undo) or overwrite it.
- moved or deleted: the tab keeps the text ryad had and offers "save as".
- not valid utf-8: ryad offers to open it as windows-1251. text that does not fit back into windows-1251 is offered as utf-8.
- files with zero bytes (binary) are not opened. the largest file ryad opens is 64 mb.

## open from explorer

`ryad.exe file.txt other.log` opens the files in the running ryad (one window, one process). the installer adds ryad to "open with" for `.txt .log .md .ini .cfg .json .csv .xml`; any other text file opens through `ctrl+o`, drag and drop or the command line. ryad does not make itself the default app: windows settings › apps › default apps lets you choose it.

## install

`ryad-0.1.0-setup.exe` installs ryad for you only, in `%LOCALAPPDATA%\Programs\ryad`, with a start menu entry and an entry in windows settings › apps. no administrator rights. running it again updates ryad: a running ryad is closed first and saves its session.

**replace windows notepad** (a switch in the installer, off by default): ryad removes the notepad app for your account and opens windows settings at ryad's default apps page, where one click makes it the default for text files. windows does not let programs make themselves the default; that click is yours. the old `notepad.exe` that is part of windows itself needs administrator rights to remove and is left alone. uninstalling ryad offers notepad back (its microsoft store page opens).

portable: copy `ryad.exe` anywhere and run it. the drafts go to the same place either way.

only a file named `ryad-setup.exe` or `ryad-<version>-setup.exe` runs the installer; any other name runs ryad.

## uninstall

windows settings › apps › ryad › uninstall. it removes the program, the start menu entry and the "open with" entries, and, if ryad replaced windows notepad, offers it back. **your drafts and settings stay** in `%LOCALAPPDATA%\ryad`, so a later install brings the session back.

## where your text lives

ryad works offline: no network, no account, no telemetry, no update checks.

unsaved text is stored in `%LOCALAPPDATA%\ryad\session` as plain utf-8 files, readable by anyone who can read your user profile. it stays there after closing ryad and after uninstalling it. to remove it: discard the changes in each tab (or save them to files), use file › drafts › delete all kept drafts, then quit ryad and delete `%LOCALAPPDATA%\ryad`.

`--trace` prints timings, sizes and revision numbers; never text, search terms or clipboard contents.

## resource use

measured on one machine (windows 11, ryzen 7 8845hs, 150 %), release build; details in [docs/measurements.md](docs/measurements.md):

| | |
|---|---|
| start → ready to type | ~82 ms |
| idle | 1 process, ~2.7 mb private memory, 0 % cpu |
| open a 1 / 10 / 50 mb log | ~0.12 / 0.76 / 4.2 s |
| ryad.exe (also the installer) | ~0.6 mb |

these are this machine's numbers, not guarantees.

## limitations

not verified, so not claimed to work: keyboard shortcuts pressed on a real keyboard (the tests send the commands as messages), windows 10, signing out or shutting down with ryad open (ryad saves its session when windows asks, but this was not tested), scaling other than 150 % and moving between monitors with different scaling, high contrast, ime input, screen readers beyond the text field and buttons, drag and drop from explorer, the real "open with" flow. see [docs/testing.md](docs/testing.md).

known:

- emoji are monochrome; in files over ~128k characters, emoji that were already in the file show as boxes (the text itself is intact; typed emoji show normally)
- large files take time: about 0.8 s for 10 mb and 4 s for 50 mb, during which the window waits. a file with very long lines opens with wrap on
- after editing, a file with mixed line endings is saved with the most common one (the status line says "mixed → crlf" before you save)
- files with u+2028 / u+2029 separators are not opened: the windows text control would change them
- a vertical tab breaks a line on screen but is not a new line for the line numbers
- the menu bar is drawn by ryad (the windows one cannot be themed): screen readers do not see it as a menu bar; the menus that open from it are normal windows menus
- the markdown preview shows links but does not open them; html, footnotes, reference-style links and math show as plain text
- the tab strip is not visible to screen readers as a list of tabs; the window title names the active tab and the tabs menu lists all of them
- `ctrl+tab` follows the tab order, not the order of use
- not code-signed: windows smartscreen may warn on first start

## build from source

```powershell
cargo build --release --target x86_64-pc-windows-msvc
cargo test --release
powershell -NoProfile -ExecutionPolicy Bypass -File tests\session.ps1
powershell -ExecutionPolicy Bypass -File tools\release.ps1     # dist\: zip, setup.exe, sha256
```

needs rust (stable, msvc toolchain). one dependency: `windows` (microsoft's windows-rs). the test scripts never press keys or move the mouse, and always use their own data folder.

## documentation

- [docs/architecture.md](docs/architecture.md) — decisions and why, the session protocol, file safety
- [docs/measurements.md](docs/measurements.md) — how and what was measured
- [docs/testing.md](docs/testing.md) — what is verified and what is not
- [research/spikes/README.md](research/spikes/README.md) — the text control spike

## license

MIT, see [LICENSE](LICENSE).
