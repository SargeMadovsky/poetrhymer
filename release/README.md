# Выпуск Windows

1. Увеличить версию в Cargo.toml и README, обновить Cargo.lock.
2. Установить Inno Setup и Python `cryptography`.
3. Выполнить `./release/build.ps1 -SigningSeed <локальный-путь-к-seed> -Iscc <путь-к-ISCC.exe>`.
4. Проверить установщик, приложение и обновление, затем создать tag `vVERSION` и GitHub Release. Загрузить только файлы из `dist`.

Seed — 32 случайных байта. Для первого выпуска он создан локально в игнорируемом `secrets/update-signing.seed`. Он не включён в Git, EXE или релиз. Сохраните резервную копию ключа в защищённом хранилище: без него нельзя подписывать следующие обновления для установленных копий. Не создавайте новый ключ для каждого выпуска.

Публичный ключ `update-public-key.hex` встроен в приложение. При его смене нужна отдельная процедура миграции доверия. Скрипт подписи сверяет seed с этим ключом, подписывает точные байты манифеста и EXE и проверяет созданные подписи.

CI собирает и тестирует исходники, но не имеет ключа и не публикует подписанные релизы. Выпуск подписывается локально. Не передавайте ключ в открытые workflow, логи или артефакты. Для автоматизации подписания позже можно настроить защищённый GitHub Environment.

Отдельную подпись EXE можно проверить Python-библиотекой cryptography:

```python
from pathlib import Path
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
key = Ed25519PublicKey.from_public_bytes(bytes.fromhex(Path("release/update-public-key.hex").read_text().strip()))
key.verify(Path("Poetrhymer.exe.sig").read_bytes(), Path("Poetrhymer.exe").read_bytes())
```

Обновлятор проверяет подпись манифеста до разбора его полей, затем размер и SHA-256 EXE перед заменой. Первая установка требует доверия к источнику загрузки: Ed25519 не заменяет Authenticode или репутацию SmartScreen.
