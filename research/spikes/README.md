# research/spikes

одноразові прототипи, не частина збірки ryad. кожен з них збирається окремо
(`cargo build --release` у своїй папці; власний `[workspace]`, власний `target`).

## richedit/

перевірка системного rich edit (`msftedit.dll`, клас `RICHEDIT50W`) у режимі
plain text як текстового поля ryad. 2026-11-14, windows 11 home 26200,
`msftedit.dll` 10.0.26100.9949, ryzen 7 8845hs, release-збірка spike.

```powershell
richedit-spike.exe                 # перевірки: вірність тексту, одиниці, перенос, undo, ввід, пам'ять
richedit-spike.exe --big           # 1/10/50 MiB коротких рядків, довгий рядок
richedit-spike.exe --long2         # що саме робить довгий рядок повільним
$env:SPIKE_CLEAR = '2'             # які біти IMF_* зняти (hex), за замовчуванням 92
```

| файл | що там |
|---|---|
| `richedit-checks.md` | таблиця перевірок (без IMF_AUTOFONT) |
| `richedit-big-run1.md` | перший прогін великих файлів, IMF_AUTOFONT увімкнено (за замовчуванням rich edit): 10 MiB змішаного тексту одним рядком — 564 с |
| `richedit-long-autofont.md` | довгий рядок 64 KiB / 256 KiB / 1 MiB з IMF_AUTOFONT: 33 мс / 369 мс / 5.2 с — росте швидше за лінійно |
| `richedit-big.md` | великі файли без IMF_AUTOFONT: усе лінійне |

висновки й рішення — `docs/architecture.md`, розділ 3.

знімок вікна spike через `PrintWindow` вийшов порожнім: вікно стояло за
межами екрана. вигляд перевіряли вже на вікні ryad (`tests/common.ps1`,
`Save-Shot`).
