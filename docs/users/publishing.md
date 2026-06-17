# Publish elfshaker packs

Use a remote index (`.esi` file) to publish packs for `elfshaker clone` and
`elfshaker update`. The index is a small catalog containing each pack's URL
and checksums. ESI v2 also records pack and pack-index sizes.

This guide publishes both supported index formats:

- `index.esi` uses ESI v1 and remains the compatibility endpoint for older
  clients.
- `index.v2.esi` uses ESI v2. Current clients try this sibling first when they
  clone from the unversioned `index.esi` URL.

The `elfshaker esi` commands edit local files. Your release or hosting workflow
uploads them.

## Prepare the files

Start with a pack and its adjacent pack index:

```text
release.pack
release.pack.idx
```

`elfshaker pack release` normally creates `packs/release.pack` and
`packs/release.pack.idx` inside the selected data directory. Copy or stage both
files in the directory where you prepare the publication.

Keep these files together while editing the remote indexes. Choose the public
directory that will contain them. The examples below use:

```text
https://downloads.example.test/toolchains/
```

Use unique pack filenames for new releases. Immutable pack URLs avoid cache
ambiguity and make a published index easier to reproduce.

## Create the remote indexes

Create the v1 compatibility index at the unversioned URL:

```sh
elfshaker esi create \
  --meta v1 \
  --url https://downloads.example.test/toolchains/index.esi \
  index.esi
```

Create the v2 index at the matching versioned URL:

```sh
elfshaker esi create \
  --meta v2 \
  --url https://downloads.example.test/toolchains/index.v2.esi \
  index.v2.esi
```

The URL stored in each index is the base used to resolve relative pack URLs.
Keep the unversioned endpoint on v1; do not replace it with the newest format.

## Add a pack to both formats

Add the pack to both indexes in one operation:

```sh
elfshaker esi add \
  --also index.esi \
  index.v2.esi \
  release.pack
```

Elfshaker reads `release.pack` and `release.pack.idx`, calculates their
checksums and sizes once, and writes the fields required by each index version.
The stored pack URL defaults to `release.pack`, relative to the index URL.

If the published pack has a different path, set the stored URL explicitly:

```sh
elfshaker esi add \
  --pack-url packs/release.pack \
  --also index.esi \
  index.v2.esi \
  release.pack
```

`--pack-url` accepts one pack at a time. Relative URLs are resolved against the
public index URL; absolute URLs may point to another host.

## Inspect the result

List each index before uploading it:

```sh
elfshaker esi list index.esi
elfshaker esi list index.v2.esi
```

Each v1 line contains the pack-index checksum, pack checksum, and pack URL. A
v2 line appends the pack size and pack-index size in bytes.

The editor writes the previous contents of every changed index to a `.bak`
file. Keep those backups locally until the publication is verified. Do not
upload them.

## Upload in a safe order

Upload the immutable data before the indexes that advertise it:

1. Upload `release.pack` and `release.pack.idx`.
2. Confirm both files are available at the URLs implied by the index.
3. Upload `index.v2.esi` and `index.esi`, replacing each index atomically when
   the hosting system supports it.

This order prevents clients from seeing an index entry before its pack files
are available.

## Test the published entrypoint

Clone from the unversioned compatibility URL into a new directory:

```sh
work=$(mktemp -d)
cd "$work"
elfshaker clone \
  https://downloads.example.test/toolchains/index.esi \
  published-check
cd published-check
elfshaker list
```

A current client requests `index.v2.esi` first. It uses `index.esi` only when
the v2 sibling returns HTTP 404. Extract one listed snapshot to verify the pack
and pack index:

```sh
elfshaker extract --reset --verify PACK:SNAPSHOT
```

## Publish later packs

Existing repositories keep the endpoint selected when their remote was
created. Publishing v2 leaves existing v1 repositories on v1 during ordinary
updates, so older clients can continue to use those repositories.

Retain the current `index.esi` and `index.v2.esi` as release inputs. For each
new pack:

1. Put the new `.pack` and `.pack.idx` beside the indexes.
2. Run `elfshaker esi add --also index.esi index.v2.esi NEW.pack`.
3. Inspect both indexes with `elfshaker esi list`.
4. Upload the new pack files first, then upload both updated indexes.

To stop advertising a pack, remove its exact stored URL from both indexes:

```sh
elfshaker esi remove \
  --also index.esi \
  index.v2.esi \
  release.pack
```

Publish the changed indexes before deleting pack data. Existing clients may
still refer to an older copy of the index, so choose a retention period that
matches the surrounding release policy.
