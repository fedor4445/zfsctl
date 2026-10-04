# Real SMB Support (smb4.conf) Implementation Design

## Summary

Replaces `zfsctl`'s current, non-functional SMB share handling (`Share::Smb`, derived from ZFS's `sharesmb` property, which is confirmed to never actually work on FreeBSD — `zfs set sharesmb=on` prints "No SMB support in FreeBSD yet." and fails, even though the property value gets stored) with real, working SMB sharing: `zfsctl` manages its own clearly-delimited block inside `/usr/local/etc/smb4.conf`, scoped to datasets, and actually makes a created share connectable from a Windows PC — including the Samba-specific user password a share's "valid users" restriction needs, which is a separate credential from the user's Unix password and easy to silently skip (confirmed live: `fb`'s existing hand-configured shares needed this and it wasn't obvious at a glance whether it was in place).

This is the project's third attempt at SMB, after `sharesmb` (confirmed broken) and the subsequent view/remove-only `sharesmb`-based UI (confirmed to show a property that never did anything). It directly delivers on the user's own long-standing goal, stated earlier this project: "SMB will have to be included at some point: Windows is still number 1 Desktop OS."

## Goals

- `'M'` in the Datasets section (Middle pane focus, mirroring `'N'` for NFS) opens a create/edit SMB share form for the selected dataset.
- A created share is **actually connectable** from a Windows PC immediately — not just a correctly-formatted config stanza. This means: Samba is told to reload after any change, and if the share restricts access to a specific user, that user's Samba password is set as part of creating the share (prompted for in the same form), not left as a manual follow-up step the user has to remember.
- `zfsctl` owns and exclusively manages one clearly-delimited block inside `smb4.conf` (marker-comment-bounded, the same technique already proven on this exact box for `pf.conf`'s Bastille-templated section). Everything outside that block — including `fb`'s existing hand-configured `[alice]`/`[carol]` shares, confirmed live to not even correspond to a ZFS dataset — is never read, never touched, and never at risk of being overwritten.
- The Shares section's existing SMB row (currently sourced from the dead `sharesmb` property) is replaced with real rows sourced from `zfsctl`'s managed block. `Enter` edits, `d` removes — same convention NFS already uses.

## Non-goals

- **Not a general `smb4.conf` editor.** Shares outside `zfsctl`'s own managed block are never shown, parsed, or modified — matches the TrueNAS CORE / XigmaNAS precedent (both platforms own SMB config as a generated artifact from their own internal model, never trying to parse-and-respect hand-edited stanzas) scoped down to "owns its own marked section," since unlike those platforms `zfsctl` must coexist with a `smb4.conf` that has real, independently-managed content outside its block.
- **Not a user-management tool.** A share's "valid user" must already exist as a real Unix account — `zfsctl` never runs `pw useradd` or similar, matching the same assumption Permissions/ACL's `user:<name>` fields already make (confirmed: no user-creation feature exists anywhere in the app today).
- **One user per share, not a list.** Real `smb4.conf` syntax allows `valid users = a, b, c`, but each user needs their own, independently-set Samba password — supporting a list would mean prompting for (and setting) multiple passwords in one form. Scoped to exactly one optional "Valid user" field for this pass; a blank value means no restriction (guest-accessible, via the existing `map to guest = Bad User` global setting already present in `fb`'s `[global]` section). Multi-user shares are a clearly separable future enhancement, not a late addition to this one.
- **`browseable` is not a form field**, hardcoded to `yes` (every real share seen on `fb` already uses this) — a deliberate simplification to keep the form to three fields, not a feature anyone asked for.

## Architecture

### Current state (confirmed by reading the code and the real box)

- `src/zfs/share.rs`: `Share::Smb { dataset: String, name: Option<String> }` is parsed from the dead `sharesmb` property. `samba_available(runner)` already exists (`service samba_server onestatus`) and is reused as-is — this check doesn't change.
- `fb@172.16.10.90` already has Samba installed and running (`samba423-4.23.8_1`, `smbd`/`nmbd` both up) with a real, hand-edited `/usr/local/etc/smb4.conf`:
  ```
  [global]
      workgroup = WORKGROUP
      security = user
      map to guest = Bad User
      interfaces = lo tailscale0
      bind interfaces only = yes

  [alice]
      path = /data/share/alice
      valid users = alice
      browseable = yes
      read only = no

  [carol]
      path = /data/share/carol
      valid users = carol
      browseable = yes
      read only = no
  ```
  Confirmed live: neither `/data/share/alice` nor `/data/share/carol` corresponds to any ZFS dataset's mountpoint (checked every dataset on the box — nothing is mounted under `/data` at all). These two shares are explicitly out of scope for this feature (see Non-goals) and must never be touched.
- Confirmed live: `alice`/`carol`/a disposable test user all have working Samba passwords (`pdbedit -L` lists them), and the non-interactive password-setting invocation is:
  ```
  printf '<password>\n<password>\n' | smbpasswd -a -s <username>
  ```
  (confirmed live, `EXIT:0`, `pdbedit -L` showed the new entry afterward). `-a` adds the user to Samba's password database (the Unix account must already exist — standard `smbpasswd` behavior); `-s` reads the (doubled, for confirmation) password from stdin instead of prompting interactively, the same "non-interactive, no hang" philosophy already established for `ssh`'s `BatchMode=yes` and `sudo`'s `-n`.

### What's new

1. **Managed-block parsing and writing** (new module, `src/zfs/smb.rs`): reads `/usr/local/etc/smb4.conf`, locates a marker-comment-delimited block:
   ```
   ## zfsctl managed shares -- do not edit by hand -- start
   [sharename]
       path = /tank/data
       valid users = someuser
       browseable = yes
       read only = no
   ## zfsctl managed shares -- do not edit by hand -- end
   ```
   parses every `[stanza]` found strictly between the two marker lines into a `SmbShare { name: String, dataset_path: String, valid_user: Option<String>, read_only: bool }`, and ignores everything outside them completely (including `[global]` and any hand-edited shares). Writing (create/edit/remove) always rewrites the *entire block* from the current in-memory list of managed shares, leaving every byte outside the markers untouched; if the markers don't exist yet (first-ever managed share on a given host), they're appended to the end of the file.
2. **Reload after any change**: every create/edit/remove calls `service samba_server reload` after rewriting `smb4.conf` (exact subcommand to be confirmed live during implementation — FreeBSD's `rc.d` convention generally supports `reload` for daemons that honor `SIGHUP`, and `smbd` does, but this gets a live check before being relied on, same as everything else in this spec).
3. **Password handling**: creating or editing a share with a non-blank "Valid user" field also requires a non-blank "Password" field (on *create*); on *edit*, a blank Password means "leave the existing Samba password alone," a non-blank one resets it. The password is piped directly to `smbpasswd -a -s <user>` as shown above — never logged, never stored in `zfsctl`'s own state longer than the single mutation call needs it.
4. **UI**: `'M'` in Datasets (Middle focus) opens the create/edit form: `[Share name, Valid user (blank = guest-accessible), Password (blank on edit = unchanged), Read-only (y/n)]`. **One managed SMB share per dataset**, same 1:1 model NFS already has (`sharenfs` is a single property on the dataset) — `'M'` on a dataset that already has a managed share edits it (pre-filled, same convention as NFS); on one that doesn't, creates it. The dataset, not the share name, is the identity used to find which managed stanza to edit/replace/remove; "Share name" is just the `[stanza]` label Windows clients see (`\\host\<sharename>`), validated as non-blank and restricted to characters Samba accepts in a share name (letters, digits, `-`, `_` — no spaces or `[`/`]`). If "Valid user" is blank, "Password" is ignored even if filled in — no `smbpasswd` call happens, since there's no user to set a password for. The Shares section's existing row rendering for SMB switches from `Share::Smb`'s `sharesmb`-property source to the new `SmbShare` list; `Enter` edits (pre-filled, same convention as NFS), `d` removes (y/n confirm, same convention as NFS).
5. **Retiring the old mechanism**: the `sharesmb`-property-based `Share::Smb` variant, its parsing, and its view/remove-only UI are removed entirely — it never did anything real to begin with (confirmed empirically in an earlier session), so there's no working behavior to preserve.

## Data flow (end to end, creating a share)

```
'M' in Datasets (dataset: tank/media, mountpoint: /tank/media)
  -> Form: [Share name, Valid user, Password, Read-only]
  -> submit, validated (Share name required; Password required iff Valid user is non-blank)
     -> read smb4.conf, locate/create the managed block
     -> append the new [sharename] stanza (path = /tank/media, valid users = <user> if given, browseable = yes, read only = <y/n>)
     -> write smb4.conf back
     -> if Valid user given: printf '<pw>\n<pw>\n' | smbpasswd -a -s <user>
     -> service samba_server reload
  -> on success: share appears in the Shares section, connectable from Windows at \\<host>\<sharename>
  -> on any failure (bad username for smbpasswd, reload failure, etc.): surfaced via status message, matching every other mutation's error-handling convention
```

## Testing

- Pure parsing/writing tests for the managed-block logic in `src/zfs/smb.rs` (no real Samba needed): parsing a fixture with managed + unmanaged stanzas correctly extracts only the managed ones; writing correctly round-trips and never touches bytes outside the markers (a fixture modeling `fb`'s real `[global]`/`[alice]`/`[carol]` content, confirmed to survive byte-for-byte after an unrelated managed-share edit).
- Command-building tests for `smbpasswd -a -s` invocation and `service samba_server reload`, same `FakeRunner`-based style as every other mutation in this app.
- Form validation tests: blank Share name rejected; a Share name containing a space or `[`/`]` rejected; non-blank Valid user with blank Password rejected on create, accepted on edit (meaning "don't change the password"); blank Valid user accepted regardless of Password's value (guest-accessible share, `smbpasswd` never called); editing a dataset that already has a managed share pre-fills the form from that share, not a blank one.
- Live verification (can't be unit-tested): confirm `service samba_server reload` is the right subcommand (vs. `restart`) on the real box before relying on it; confirm a share created through the TUI is actually mountable from a real Windows client, or at minimum `smbclient -L localhost -U <user>` against it locally — the same class of end-to-end check that caught `sharesmb`/`sharenfs` being broken in the first place, and that this spec's whole premise is responding to.

## Review focus

- The managed block's find/replace-on-write logic must be exact: a bug that writes the block in the wrong place, or that fails to find an existing block and appends a *second* one, would corrupt `smb4.conf` for a real, already-running Samba instance (as opposed to most of this app's other mutations, which operate on ZFS state that's easy to inspect and undo).
- A share's "Valid user" naming a Unix account that doesn't actually exist must fail clearly (the `smbpasswd -a -s` call itself will fail for a nonexistent user) rather than silently writing a stanza that can never authenticate.
- Editing a share to blank out "Valid user" (making it guest-accessible) should probably also warn or at least not silently leave a now-orphaned Samba password for that user sitting in `passdb.tdb` — worth a conscious decision during implementation, not an accidental gap.
- The reload step failing (e.g., a syntax the parser/writer got subtly wrong) must be surfaced clearly and distinctly from "the config write itself failed" — a share could end up written to disk but not actually live until the next reload, which is a confusing state if not reported precisely.
- Removing a managed share must leave every other managed share, and everything outside the block, completely untouched — the project's established convention for ACL's position-indexed removal already got this right once; the same rigor applies here even though removal here is by share name, not position.
