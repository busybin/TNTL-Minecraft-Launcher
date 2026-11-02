# Data safety

Two mechanisms keep a bad install or a wrong click from destroying an instance.

## Recycle bin

Deleting a world, mod, resource pack, shader pack or world backup moves it to
`<instance>/.trash/<id>/` instead of unlinking it:

- `entry.json` records the original absolute path, kind, display name, size and
  deletion time; the payload sits next to it.
- The UI lists the bin under **Trash** in the instance sidebar. Restore and
  permanent delete are per item; **Empty bin** clears everything.
- Restore is refused while the game is running and is limited to the instance
  directory and its game directory, so a tampered manifest cannot write
  elsewhere. An occupied original path is never overwritten.
- Moves inside one filesystem are renames. A world on another drive is copied
  and then removed; the source is untouched until the copy finishes.
- Nothing is deleted automatically. The bin is counted under the instance's
  disk usage, and the user clears it explicitly.
- Instance export skips `.trash` and `.txn`.

Screenshots and crash reports are still removed directly: they are generated
files, not user content.

## Transactional pack updates

`tntl-store::SharedTransaction` wraps the existing journal in a mutex so
download workers can register every file they are about to overwrite:

1. `prepare(path)` moves an existing file into `.txn/<id>/` and marks the path
   as created by the new state.
2. Downloads, overrides, stale-file removal and the metadata writes all go
   through the same transaction.
3. `commit()` deletes the backups; any error drops the transaction and restores
   every old file while removing the new ones.

This covers the whole `.mrpack`, FTB and CurseForge update path, not only the
old-file deletion. A crash leaves a journal that `recover()` replays on the next
start. The game/loader install itself is shared between instances and is not
rolled back; `repair_instance` can rebuild it.
