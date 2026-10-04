#!/usr/bin/env python3
"""Indexes: geodata packs and nodesets to fetch, listed in one YAML file.

```
page ── index_list ────────────────────────────► front ── GET <address> ───► the index's host
page ── index_install {index, kind, name} ─────► front ── GET <entry url> ──► the file's host
front ── index_progress {index, kind, name, state, fetched, of, error} ─► every page
front: size and sha256 checked; a pack expanded as Import zip expands one, a nodeset written
```

    index: sim-mesh-examples              # a usable name
    title: sim-mesh examples
    description: |                        # what the collection is, printed as it stands
      These examples will soon include some varied geographies and nodesets.
      …
    release: sim-mesh/sim-mesh:examples   # where publish puts its files (Publishing)
    geodata:
      - name: berlin-mitte                # the name it is installed under
        title: Berlin Mitte, dense flat city
        url: berlin-mitte.zip             # a sim-mesh geodata pack, relative to the index
        sha256: 3f1c…                     # 64 hex digits: what the file is
        bytes: 9400000                    # its size, which the page shows before fetching
        bbox: [13.36, 52.50, 13.44, 52.54]
        licences: ODbL 1.0; dl-de/zero-2.0; CC BY 4.0; Copernicus
        tags: [standard, urban, flat]
        description: …
    nodesets:
      - name: mitte-40
        title: 40 rooftop nodes in Mitte
        url: mitte-40.yaml                # a nodeset file
        sha256: 9ab0…
        bytes: 5210
        geodata: berlin-mitte             # the ground it is made for
        nodes: 40

`name`, `url` and `sha256` are an entry's own; everything else is what the
page shows. The index's `description` is text about the whole collection,
which the page shows under the index's name and `sim index list` prints,
its line breaks kept. A key this module does not know is passed over.

**An entry is immutable.** Its sha256 is what it is, and two machines that
installed it stand on the same bytes, which is what makes a test on standard
ground comparable between them. A pack or nodeset that changes is published
under a new name.

**An address** is an http(s) URL, a `file://` URL or a local path; one
ending in `/` means the `index.yaml` in it. An entry's `url` is relative to
the address, so anyone can publish a directory holding an index and its
files: a web site, a release, a directory on a USB stick.

**sim-mesh's own index**, `sim-mesh-examples`, is always listed (its address
is SIM_MESH_INDEX, else sim-mesh.net/examples/index.yaml); the ones a person
adds are kept in
`testbed/indexes.yaml`, each with the name it gave when added:

    - { name: someone-elses, address: "https://example.org/mesh/index.yaml" }

**Installed, an entry remembers where it came from**: geodata in
`geodata/<name>/.origin.yaml`, a nodeset in `nodesets/.origin/<name>.yaml`,
each {index, address, url, sha256, added}. An entry is installed when that
origin's sha256 is its own. A name already taken by other ground or another
nodeset is refused, with the sentence saying so, and nothing is replaced. A
nodeset whose own file no longer has the sha256 it came with has been
changed here, and says so.

**A nodeset names the geodata it is made for.** Installing one whose geodata
the same index offers, and which is not installed, installs that first.

**Publishing** adds something installed here to an index checked out here:

```
sim geodata publish berlin --as berlin-centre --index ../sim-mesh.github.io/examples
  ─► export_zip ─► sha256, bytes, bbox, licences
  ─► GET/POST api.github.com/repos/<owner/repo>/releases(/tags/<tag>)   the index's `release:`
  ─► POST uploads.github.com/…/assets?name=berlin-centre.zip           refused if there already
  ─► the entry appended to the index file, which the person commits and pushes
```

An index without `release:` gets the file beside it, its `url` relative. An
entry the index lists already, or a file of its name already there, is
refused: an entry never changes. The upload's token is GH_TOKEN (GITHUB_TOKEN
too); `sim` takes it from gh where it runs when neither is set. The index
file is written back whole: its leading comments, its own keys, then every
entry's keys in one order.

Downloads are aiohttp in the caller's loop, streamed to a `.part-` file and
hashed as they come; the file is checked and expanded on a worker thread, so
nothing here blocks an event loop. Run as a script it is the CLI behind
`sim index`, `sim geodata` and `sim nodeset`.
"""

import argparse
import asyncio
import contextlib
import datetime
import hashlib
import os
import re
import shutil
import sys
import tempfile
import urllib.parse
import urllib.request

import yaml

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import geodata as geodata_module   # noqa: E402 - the path is set just above
import nodeset as nodeset_module   # noqa: E402
import store                       # noqa: E402

OWN_NAME = "sim-mesh-examples"
OWN_ADDRESS = os.environ.get("SIM_MESH_INDEX", "https://sim-mesh.net/examples/index.yaml")
ADDED_FILE = os.path.join(store.SIM_DIR, "indexes.yaml")
INDEX_FILE = "index.yaml"            # what an address ending in `/` means
ORIGIN_FILE = ".origin.yaml"         # in a geodata's directory
GEODATA, NODESETS = "geodata", "nodesets"
KINDS = (GEODATA, NODESETS)
SHA_RE = re.compile(r"^[0-9a-f]{64}$")
MAX_INDEX_BYTES = 4 << 20
MAX_NODESET_BYTES = 64 << 20
CHUNK = 1 << 16
LIST_TIMEOUT_S = 20
FETCH_TIMEOUT_S = 3600
TEXT_KEYS = ("title", "description", "licences")


class IndexFault(store.StoreError):
    """An index that cannot be read, or an entry that cannot be installed.
    The message is meant for the page as is."""


def now_utc():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


# ---- addresses ---------------------------------------------------------------

def is_url(text):
    return re.match(r"^(https?|file)://", text or "") is not None


def resolve(address):
    """An address as it is fetched: a local path absolute, and one ending in
    `/` (a directory, for a path) its index.yaml."""
    address = str(address or "").strip()
    if not address:
        raise IndexFault("an index's address is a URL or a path")
    if is_url(address):
        return address + INDEX_FILE if address.endswith("/") else address
    path = caller_path(address)
    return os.path.join(path, INDEX_FILE) if os.path.isdir(path) else path


def caller_path(path):
    """A path a person gave, absolute: a relative one is read from where
    `sim` was run (SIM_MESH_CALLER_DIR, which it passes into its container),
    else from here."""
    path = os.path.expanduser(str(path))
    base = os.environ.get("SIM_MESH_CALLER_DIR")
    if not os.path.isabs(path) and base:
        path = os.path.join(base, path)
    return os.path.abspath(path)


def entry_url(address, url):
    """An entry's `url` as fetched: relative to its index's address."""
    url = str(url)
    if is_url(url) or not is_url(address) and os.path.isabs(url):
        return url
    if is_url(address):
        return urllib.parse.urljoin(address, url)
    return os.path.normpath(os.path.join(os.path.dirname(address), url))


def local_path(location):
    """The file a local address or a `file://` URL names; None for http(s)."""
    if location.startswith("file://"):
        return urllib.request.url2pathname(urllib.parse.urlsplit(location).path)
    return None if is_url(location) else location


def added():
    """The indexes a person added: [{name, address}], in the order added."""
    try:
        with open(ADDED_FILE, encoding="utf-8") as handle:
            rows = yaml.safe_load(handle) or []
    except FileNotFoundError:
        return []
    except (OSError, yaml.YAMLError) as err:
        raise IndexFault("%s: %s" % (ADDED_FILE, err)) from err
    return [{"name": str(r["name"]), "address": str(r["address"])}
            for r in rows if isinstance(r, dict) and r.get("name") and r.get("address")]


def _write_added(rows):
    store.write_text(ADDED_FILE, "".join(
        "- { name: %s, address: %s }\n" % (store.scalar(r["name"]), store.scalar(r["address"]))
        for r in rows))


def addresses():
    """Every index listed: sim-mesh's own first, then the added ones,
    [{name, address, own}]."""
    return ([{"name": OWN_NAME, "address": OWN_ADDRESS, "own": True}]
            + [dict(r, own=False) for r in added()])


# ---- reading an index --------------------------------------------------------

def parse(text, address):
    """An index's text, checked: {index, title, address, geodata, nodesets},
    each entry with its `url` resolved against the address."""
    try:
        data = yaml.safe_load(text)
    except yaml.YAMLError as err:
        raise IndexFault("%s: %s" % (address, err)) from err
    if not isinstance(data, dict) or not data.get("index"):
        raise IndexFault("%s is not an index: it names no `index:`" % address)
    name = str(data["index"])
    if not store.NAME_RE.match(name):
        raise IndexFault("%s: %r is not a usable index name" % (address, name))
    out = {"index": name, "title": str(data.get("title") or ""),
           "description": str(data.get("description") or "").strip(), "address": address}
    for kind in KINDS:
        rows = data.get(kind) or []
        if not isinstance(rows, list):
            raise IndexFault("%s: `%s:` is a list of entries" % (address, kind))
        seen = set()
        out[kind] = []
        for row in rows:
            entry = _entry(row, kind, address)
            if entry["name"] in seen:
                raise IndexFault("%s: two %s entries are called %s" % (address, kind, entry["name"]))
            seen.add(entry["name"])
            out[kind].append(entry)
    return out


def _entry(row, kind, address):
    where = "%s: a %s entry" % (address, "geodata" if kind == GEODATA else "nodeset")
    if not isinstance(row, dict):
        raise IndexFault("%s is not a mapping" % where)
    name = str(row.get("name") or "")
    if not store.NAME_RE.match(name):
        raise IndexFault("%s has no usable name (%r)" % (where, name))
    where = "%s: %s" % (address, name)
    if not row.get("url"):
        raise IndexFault("%s names no url" % where)
    sha = str(row.get("sha256") or "").lower()
    if not SHA_RE.match(sha):
        raise IndexFault("%s: sha256 is 64 hex digits" % where)
    out = {"name": name, "url": entry_url(address, row["url"]), "sha256": sha}
    for key in TEXT_KEYS:
        if row.get(key) is not None:
            out[key] = str(row[key])
    if row.get("bytes") is not None:
        out["bytes"] = _whole(row["bytes"], "bytes", where)
    if row.get("tags") is not None:
        tags = row["tags"]
        out["tags"] = [str(t) for t in (tags if isinstance(tags, list) else [tags])]
    if kind == GEODATA and row.get("bbox") is not None:
        box = row["bbox"]
        try:
            out["bbox"] = [float(v) for v in box]
        except (TypeError, ValueError):
            out["bbox"] = []
        if len(out["bbox"]) != 4:
            raise IndexFault("%s: bbox is [lon0, lat0, lon1, lat1]" % where)
    if kind == NODESETS:
        if row.get("geodata") is not None:
            out["geodata"] = store.check_name(str(row["geodata"]), "geodata")
        if row.get("nodes") is not None:
            out["nodes"] = _whole(row["nodes"], "nodes", where)
    return out


def _whole(value, key, where):
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise IndexFault("%s: %s is a whole number, not %r" % (where, key, value))
    return value


async def fetch_text(location, session):
    """An index's text, from its host or this machine."""
    path = local_path(location)
    if path is not None:
        def read():
            with open(path, "rb") as handle:
                return handle.read(MAX_INDEX_BYTES + 1)
        try:
            raw = await asyncio.to_thread(read)
        except OSError as err:
            raise IndexFault("%s: %s" % (location, err.strerror or err)) from err
    else:
        import aiohttp
        try:
            async with session.get(location, timeout=aiohttp.ClientTimeout(total=LIST_TIMEOUT_S)) as resp:
                if resp.status != 200:
                    raise IndexFault("%s: HTTP %d" % (location, resp.status))
                raw = await resp.content.read(MAX_INDEX_BYTES + 1)
        except (aiohttp.ClientError, asyncio.TimeoutError, OSError) as err:
            raise IndexFault("%s: %s" % (location, str(err) or type(err).__name__)) from err
    if len(raw) > MAX_INDEX_BYTES:
        raise IndexFault("%s: larger than an index may be" % location)
    return raw.decode("utf-8", "replace")


async def fetch(address, session):
    location = resolve(address)
    return parse(await fetch_text(location, session), location)


# ---- what is installed -------------------------------------------------------

def _read_origin(path):
    try:
        with open(path, encoding="utf-8") as handle:
            got = yaml.safe_load(handle)
    except (OSError, yaml.YAMLError):
        return None
    return got if isinstance(got, dict) else None


def geodata_origin(name):
    """Where installed geodata came from, {index, address, url, sha256,
    added}, or None: built, imported or made here."""
    return _read_origin(os.path.join(geodata_module.geodata_dir(name), ORIGIN_FILE))


def nodeset_origin(name):
    """Where an installed nodeset came from, or None; with `changed` true
    when its file no longer has the sha256 it came with."""
    got = _read_origin(nodeset_module.origin_path(name))
    if got is None:
        return None
    try:
        with open(nodeset_module.nodeset_path(name), "rb") as handle:
            now = hashlib.sha256(handle.read()).hexdigest()
    except OSError:
        return None
    return dict(got, changed=now != got.get("sha256"))


def _write_origin(path, index, entry):
    store.write_text(path, "".join("%s: %s\n" % (key, store.scalar(value)) for key, value in (
        ("index", index["index"]), ("address", index["address"]), ("url", entry["url"]),
        ("sha256", entry["sha256"]), ("added", now_utc()))))


def status(kind, entry):
    """Whether an entry is here: `installed` (with `changed` for a nodeset
    edited since), `taken` (the name is something else's), or neither."""
    name = entry["name"]
    if kind == GEODATA:
        exists = os.path.isfile(geodata_module.geodata_path(name))
        origin = geodata_origin(name) if exists else None
    else:
        exists = os.path.isfile(nodeset_module.nodeset_path(name))
        origin = nodeset_origin(name) if exists else None
    mine = origin is not None and origin.get("sha256") == entry["sha256"]
    out = {"installed": mine, "taken": exists and not mine}
    if mine and kind == NODESETS:
        out["changed"] = bool(origin.get("changed"))
    return out


async def listing(session):
    """Every index, fetched at once: [{name, address, own, title, error?,
    geodata, nodesets}], each entry with its status here."""
    rows = addresses()

    async def one(row):
        try:
            got = await fetch(row["address"], session)
        except store.StoreError as err:
            return dict(row, title="", description="", error=str(err), geodata=[], nodesets=[])
        out = dict(row, title=got["title"], description=got["description"], geodata=[], nodesets=[])
        if got["index"] != row["name"]:
            out["error"] = "the index at this address is now called %s" % got["index"]
        for kind in KINDS:
            out[kind] = [dict(e, **status(kind, e)) for e in got[kind]]
        return out
    return list(await asyncio.gather(*(one(r) for r in rows)))


# ---- adding and removing an index --------------------------------------------

async def add_index(address, session):
    """An index added by its address, read first: the name it gives must be
    new among the listed ones. Returns its row."""
    got = await fetch(address, session)
    taken = {r["name"] for r in addresses()}
    if got["index"] in taken:
        raise IndexFault("an index called %s is already listed" % got["index"])
    rows = added() + [{"name": got["index"], "address": str(address).strip()}]
    _write_added(rows)
    return {"name": got["index"], "address": str(address).strip(), "own": False,
            "title": got["title"], "description": got["description"]}


def remove_index(name):
    """An added index forgotten, by its name or address; what came from it
    stays installed. sim-mesh's own is always listed."""
    if name in (OWN_NAME, OWN_ADDRESS):
        raise IndexFault("%s is sim-mesh's own index, always listed" % OWN_NAME)
    rows = added()
    kept = [r for r in rows if name not in (r["name"], r["address"])]
    if len(kept) == len(rows):
        raise IndexFault("no index called %s is listed" % name)
    _write_added(kept)
    return [r["name"] for r in rows if r not in kept]


# ---- installing an entry ------------------------------------------------------

async def _download(url, dest, sha, size, session, progress):
    """A file into `dest`, hashed as it comes; refused when its size or its
    sha256 is not the entry's."""
    digest = hashlib.sha256()
    fetched = 0
    path = local_path(url)
    if path is not None:
        def copy():
            nonlocal fetched
            with open(path, "rb") as src, open(dest, "wb") as dst:
                while True:
                    chunk = src.read(CHUNK)
                    if not chunk:
                        return
                    digest.update(chunk)
                    dst.write(chunk)
                    fetched += len(chunk)
        try:
            await asyncio.to_thread(copy)
        except OSError as err:
            raise IndexFault("%s: %s" % (url, err.strerror or err)) from err
        if progress:
            progress(fetched, size)
    else:
        import aiohttp
        try:
            async with session.get(url, timeout=aiohttp.ClientTimeout(
                    total=FETCH_TIMEOUT_S, sock_read=120)) as resp:
                if resp.status != 200:
                    raise IndexFault("%s: HTTP %d" % (url, resp.status))
                of = size or resp.content_length
                with open(dest, "wb") as handle:
                    async for chunk in resp.content.iter_chunked(CHUNK):
                        digest.update(chunk)
                        handle.write(chunk)
                        fetched += len(chunk)
                        if size is not None and fetched > size:
                            raise IndexFault("%s is larger than the index says (%d bytes)"
                                              % (url, size))
                        if progress:
                            progress(fetched, of)
        except (aiohttp.ClientError, asyncio.TimeoutError, OSError) as err:
            raise IndexFault("%s: %s" % (url, str(err) or type(err).__name__)) from err
    if size is not None and fetched != size:
        raise IndexFault("%s is %d bytes, and the index says %d" % (url, fetched, size))
    if digest.hexdigest() != sha:
        raise IndexFault("%s is not the file the index lists: its sha256 differs" % url)


def find(index, kind, name):
    for entry in index[kind]:
        if entry["name"] == name:
            return entry
    raise IndexFault("%s offers no %s called %s" % (
        index["index"], "geodata" if kind == GEODATA else "nodeset", name))


async def index_named(name, session):
    for row in addresses():
        if row["name"] == name:
            return await fetch(row["address"], session)
    raise IndexFault("no index called %s is listed" % name)


async def install(index_name, kind, name, session, progress=None):
    """One entry of a listed index installed under its own name: {kind,
    name, installed: [what was installed, in order]}. A nodeset's geodata
    from the same index comes first when it is not here and its name is free."""
    if kind not in KINDS:
        raise IndexFault("an entry is geodata or nodesets, not %s" % kind)
    index = await index_named(index_name, session)
    entry = find(index, kind, name)
    done = []
    if kind == NODESETS and entry.get("geodata"):
        ground = next((g for g in index[GEODATA] if g["name"] == entry["geodata"]), None)
        if ground is not None and not any(status(GEODATA, ground).values()):
            await _install_one(index, GEODATA, ground, session, progress)
            done.append({"kind": GEODATA, "name": ground["name"]})
    await _install_one(index, kind, entry, session, progress)
    done.append({"kind": kind, "name": name})
    return {"kind": kind, "name": name, "installed": done}


async def _install_one(index, kind, entry, session, progress):
    name = entry["name"]
    here = status(kind, entry)
    if here["installed"]:
        return
    if here["taken"]:
        raise IndexFault("there is already %s called %s, not the one %s offers: rename or "
                          "delete it first" % ("geodata" if kind == GEODATA else "a nodeset",
                                               name, index["index"]))
    base = store.GEODATA_DIR if kind == GEODATA else store.NODESETS_DIR
    os.makedirs(base, exist_ok=True)
    handle, tmp = tempfile.mkstemp(prefix=geodata_module.PART_PREFIX, dir=base)
    os.close(handle)
    tell = (lambda fetched, of: progress(kind, name, fetched, of)) if progress else None
    try:
        await _download(entry["url"], tmp, entry["sha256"], entry.get("bytes"), session, tell)
        if kind == GEODATA:
            await asyncio.to_thread(_place_geodata, tmp, index, entry)
        else:
            await asyncio.to_thread(_place_nodeset, tmp, index, entry)
    finally:
        with contextlib.suppress(OSError):
            os.unlink(tmp)


def _place_geodata(zip_path, index, entry):
    geodata_module.import_zip(zip_path, entry["name"])
    _write_origin(os.path.join(geodata_module.geodata_dir(entry["name"]), ORIGIN_FILE),
                  index, entry)


def _place_nodeset(path, index, entry):
    if os.path.getsize(path) > MAX_NODESET_BYTES:
        raise IndexFault("%s is larger than a nodeset may be" % entry["url"])
    nodeset_module.read(path)              # a nodeset, or the reason it is not one
    dest = nodeset_module.nodeset_path(entry["name"])
    if os.path.exists(dest):
        raise IndexFault("there is already a nodeset called %s" % entry["name"])
    _write_origin(nodeset_module.origin_path(entry["name"]), index, entry)
    shutil.move(path, dest)


# ---- publishing ----------------------------------------------------------------

RELEASE_RE = re.compile(r"^([\w.-]+/[\w.-]+):([\w.-]+)$")
GITHUB_API = os.environ.get("SIM_MESH_GITHUB_API", "https://api.github.com")
UPLOAD_TIMEOUT_S = 3600
# An entry's keys in the order an index file is written in.
ENTRY_KEYS = ("name", "title", "description", "url", "sha256", "bytes", "bbox", "licences",
              "tags", "geodata", "nodes")
HEAD_KEYS = ("index", "title", "description", "release")


def index_file(path):
    """A local index's file: a directory means its index.yaml."""
    path = caller_path(path)
    if os.path.isdir(path):
        path = os.path.join(path, INDEX_FILE)
    if not os.path.isfile(path):
        raise IndexFault("%s: no index file there" % path)
    return path


def _yaml_text(key, value, indent):
    """One key of an index file: a text with line breaks as a block, a list
    or a mapping on one line, anything else a scalar."""
    pad = " " * indent
    if isinstance(value, str) and "\n" in value.strip():
        return "%s%s: |\n%s" % (pad, key, "".join(
            "%s  %s\n" % (pad, line) if line else "\n" for line in value.strip().splitlines()))
    if isinstance(value, (list, tuple, dict)):
        return "%s%s: %s\n" % (pad, key, store.flow(value))
    if isinstance(value, str) and value and _reads_as(value):
        return "%s%s: %s\n" % (pad, key, value)        # bare, as a person writes it
    return "%s%s: %s\n" % (pad, key, store.scalar(value))


def _reads_as(text):
    """Whether text written bare after a key reads back as itself."""
    try:
        return yaml.safe_load("k: %s\n" % text) == {"k": text}
    except yaml.YAMLError:
        return False


def dump_index(head, data):
    """An index file's text: its leading comment lines, its own keys, any
    other key it had, then its geodata and nodesets, each entry's keys in
    ENTRY_KEYS' order."""
    out = head
    for key in HEAD_KEYS:
        if data.get(key) not in (None, ""):
            out += _yaml_text(key, data[key], 0)
    for key, value in data.items():
        if key not in HEAD_KEYS and key not in KINDS:
            out += _yaml_text(key, value, 0)
    for kind in KINDS:
        rows = data.get(kind) or []
        if not rows:
            out += "%s: []\n" % kind
            continue
        out += "%s:\n" % kind
        for row in rows:
            keys = [k for k in ENTRY_KEYS if k in row] + [k for k in row if k not in ENTRY_KEYS]
            for i, key in enumerate(keys):
                text = _yaml_text(key, row[key], 4)
                out += ("  - " + text[4:]) if i == 0 else text
    return out


def _read_index_file(path):
    """(leading comment lines, the file's mapping), the file checked as an index."""
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    parse(text, path)
    head = ""
    for line in text.splitlines(True):
        if not line.startswith("#"):
            break
        head += line
    return head, yaml.safe_load(text)


def _token():
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if not token:
        raise IndexFault("publishing to a release needs a GitHub token: set GH_TOKEN, or log in "
                         "with gh where sim runs from, which sim then takes it from")
    return token


async def _upload_release(session, release, path, filename, say):
    """A file uploaded to a GitHub release, made when the repository has
    none by that tag; an asset of that name already there is refused, never
    replaced. Returns its download address."""
    import aiohttp

    repo, tag = RELEASE_RE.match(release).groups()
    headers = {"Authorization": "Bearer " + _token(), "Accept": "application/vnd.github+json",
               "User-Agent": "sim-mesh (+https://github.com/sim-mesh/sim-mesh)"}
    base = "%s/repos/%s/releases" % (GITHUB_API.rstrip("/"), repo)
    try:
        async with session.get("%s/tags/%s" % (base, tag), headers=headers) as resp:
            if resp.status == 404:
                rel = None
            elif resp.status != 200:
                raise IndexFault("%s: release %s: HTTP %d %s" % (repo, tag, resp.status,
                                                                 (await resp.text())[:200]))
            else:
                rel = await resp.json()
        if rel is None:
            say("making release %s on %s" % (tag, repo))
            async with session.post(base, headers=headers, json={
                    "tag_name": tag, "name": tag,
                    "body": "Files that sim-mesh indexes list, each by its sha256."}) as resp:
                if resp.status != 201:
                    raise IndexFault("%s: making release %s: HTTP %d %s" % (
                        repo, tag, resp.status, (await resp.text())[:200]))
                rel = await resp.json()
        if any(a.get("name") == filename for a in rel.get("assets") or ()):
            raise IndexFault("release %s of %s already has %s: an entry never changes, so "
                             "publish it under another name" % (tag, repo, filename))
        upload = rel["upload_url"].split("{", 1)[0]
        size = os.path.getsize(path)
        say("uploading %s (%s) to %s, release %s" % (filename, store.human_bytes(size), repo, tag))
        with open(path, "rb") as body:
            async with session.post(
                    upload, params={"name": filename}, data=body,
                    headers=dict(headers, **{"Content-Type": "application/octet-stream",
                                             "Content-Length": str(size)}),
                    timeout=aiohttp.ClientTimeout(total=UPLOAD_TIMEOUT_S)) as resp:
                if resp.status != 201:
                    raise IndexFault("%s: uploading %s: HTTP %d %s" % (
                        repo, filename, resp.status, (await resp.text())[:200]))
                return (await resp.json())["browser_download_url"]
    except (aiohttp.ClientError, asyncio.TimeoutError, OSError) as err:
        raise IndexFault("%s: %s" % (repo, str(err) or type(err).__name__)) from err


async def publish(kind, name, index_path, session, entry_name=None, title=None,
                  description=None, tags=None, geodata_for=None, say=print):
    """Something installed here published in a local index: its file made
    (a geodata pack's zip, as Export zip makes one; a nodeset's file as it
    is), measured and hashed, put where the index's files go, and its entry
    added to the index file.

    Where the files go is the index's own `release: owner/repo:tag`, a
    GitHub release they are uploaded to; or, without one, the index's own
    directory, beside it. An entry the index lists already, or a file there
    already, is refused: an entry never changes."""
    if kind not in KINDS:
        raise IndexFault("what is published is geodata or nodesets, not %s" % kind)
    path = index_file(index_path)
    head, data = _read_index_file(path)
    entry_name = store.check_name(entry_name or name, "entry")
    if any(row.get("name") == entry_name for row in data.get(kind) or ()):
        raise IndexFault("%s lists %s %s already: an entry never changes, so publish it under "
                         "another name (--as)" % (path, "geodata" if kind == GEODATA else
                                                  "nodeset", entry_name))
    release = data.get("release")
    if release and not RELEASE_RE.match(str(release)):
        raise IndexFault("%s: release is owner/repo:tag, not %r" % (path, release))
    filename = entry_name + (".zip" if kind == GEODATA else ".yaml")
    entry = {"name": entry_name}
    if title:
        entry["title"] = title
    if description:
        entry["description"] = description
    handle, tmp = tempfile.mkstemp(prefix=".publish-", dir=os.path.dirname(path))
    os.close(handle)
    try:
        if kind == GEODATA:
            gd = geodata_module.load(name)
            say("exporting geodata %s" % name)

            def export():
                with open(tmp, "wb") as out:
                    geodata_module.export_zip(gd, out)
            await asyncio.to_thread(export)
            entry["bbox"] = [round(v, 4) for v in gd.bbox]
            notices = [str(n.get("source")) for n in (gd.manifest or {}).get("licenses") or ()
                       if isinstance(n, dict) and n.get("source")]
            if notices:
                entry["licences"] = "; ".join(notices)
        else:
            ns = nodeset_module.load(name)
            shutil.copyfile(nodeset_module.nodeset_path(name), tmp)
            entry["nodes"] = len(ns.nodes)
            if geodata_for:
                entry["geodata"] = store.check_name(geodata_for, "geodata")
        if tags:
            entry["tags"] = list(tags)
        with open(tmp, "rb") as handle_:
            digest = hashlib.sha256()
            for chunk in iter(lambda: handle_.read(CHUNK), b""):
                digest.update(chunk)
        entry["sha256"] = digest.hexdigest()
        entry["bytes"] = os.path.getsize(tmp)
        if release:
            entry["url"] = await _upload_release(session, str(release), tmp, filename, say)
        else:
            dest = os.path.join(os.path.dirname(path), filename)
            if os.path.exists(dest):
                raise IndexFault("%s is there already: an entry never changes, so publish it "
                                 "under another name (--as)" % dest)
            os.replace(tmp, dest)
            entry["url"] = filename
    finally:
        with contextlib.suppress(OSError):
            os.unlink(tmp)
    data.setdefault(kind, [])
    data[kind] = list(data[kind] or []) + [entry]
    text = dump_index(head, data)
    parse(text, path)
    store.write_text(path, text)
    return entry


# ---- the CLI -------------------------------------------------------------------

def _session():
    import aiohttp
    return aiohttp.ClientSession()


async def _with_session(fn, *args):
    async with _session() as session:
        return await fn(*args, session)


def _confirm(what, names, force):
    print("to delete:")
    for name in names:
        print("  " + name)
    if force:
        return True
    if not sys.stdin.isatty():
        print("not deleting without -f when nothing can confirm", file=sys.stderr)
        return False
    return input("delete %d %s? [y/N] " % (len(names), what)).strip().lower() in ("y", "yes")


def _offered(kind, substring):
    for index in asyncio.run(_with_session(lambda session: listing(session))):
        if index.get("error"):
            print("%s: %s" % (index["name"], index["error"]), file=sys.stderr)
        for e in index[kind]:
            if substring not in e["name"]:
                continue
            here = ("installed, changed here" if e.get("changed") else "installed") \
                if e["installed"] else "name taken here" if e["taken"] else ""
            size = store.human_bytes(e["bytes"]) if e.get("bytes") is not None else ""
            print("%-16s %-32s %8s  %-40s %s" % (index["name"], e["name"], size,
                                                 e.get("title", ""), here))


def _install_cli(kind, names):
    found = {}
    for index in asyncio.run(_with_session(lambda session: listing(session))):
        for e in index[kind]:
            found.setdefault(e["name"], index["name"])

    def said(_kind, name, fetched, of):
        if of:
            print("\r%s: %s of %s" % (name, store.human_bytes(fetched), store.human_bytes(of)),
                  end="", flush=True)

    for name in names:
        if name not in found:
            raise IndexFault("no listed index offers %s called %s"
                              % ("geodata" if kind == GEODATA else "a nodeset", name))
        got = asyncio.run(_with_session(
            lambda session, n=name: install(found[n], kind, n, session, said)))
        print("\r", end="")
        for each in got["installed"]:
            print("added %s %s from %s" % ("geodata" if each["kind"] == GEODATA else "nodeset",
                                           each["name"], found[name]))


def _print_description(row):
    """An index's description, its own lines kept, indented under its name."""
    text = row.get("description") or ""
    if text:
        print("".join("    %s\n" % line for line in text.splitlines()), end="")


def main(argv=None):
    ap = argparse.ArgumentParser(prog="sim", description="indexes, geodata and nodesets")
    sub = ap.add_subparsers(dest="what", required=True)

    ix = sub.add_parser("index", help="the indexes geodata and nodesets are fetched from")
    ixv = ix.add_subparsers(dest="verb", required=True)
    ixv.add_parser("list", help="every listed index, and what it offers")
    p = ixv.add_parser("add", help="list another index, by its address")
    p.add_argument("address")
    p = ixv.add_parser("delete", help="forget an added index; what came from it stays")
    p.add_argument("name")

    for what in (GEODATA, "nodeset"):
        p = sub.add_parser(what, help="the %s installed here" % what)
        v = p.add_subparsers(dest="verb", required=True)
        v.add_parser("list", help="what is installed, with its size and where it came from")
        q = v.add_parser("offered", help="what the listed indexes offer")
        q.add_argument("substring", nargs="?", default="")
        q = v.add_parser("add", help="install from a listed index by name"
                         + (", or a geodata pack zip" if what == GEODATA else ""))
        q.add_argument("names", nargs="+")
        q = v.add_parser("delete", help="delete, after listing it for confirmation")
        q.add_argument("-f", "--force", action="store_true", help="no confirmation")
        q.add_argument("names", nargs="+")
        q = v.add_parser("publish", help="add it to a local index: its file beside the index, "
                         "or uploaded to the GitHub release the index names")
        q.add_argument("name")
        q.add_argument("--index", required=True, help="the index file, or its directory")
        q.add_argument("--as", dest="entry", help="the entry's name (default: its own)")
        q.add_argument("--title")
        q.add_argument("--description")
        q.add_argument("--tags", help="comma-separated")
        if what == "nodeset":
            q.add_argument("--geodata", help="the index's geodata entry it is made for")
    args = ap.parse_args(argv)

    try:
        if args.what == "index":
            if args.verb == "add":
                row = asyncio.run(_with_session(add_index, args.address))
                print("added index %s%s" % (row["name"], ": " + row["title"] if row["title"] else ""))
                _print_description(row)
            elif args.verb == "delete":
                for name in remove_index(args.name):
                    print("forgot index %s" % name)
            else:
                for index in asyncio.run(_with_session(lambda session: listing(session))):
                    print("%s  %s%s" % (index["name"], index["address"],
                                        "   ! " + index["error"] if index.get("error") else ""))
                    if index.get("title"):
                        print("    " + index["title"])
                    _print_description(index)
                    for kind in KINDS:
                        if index[kind]:
                            print("    %s: %s" % (kind, ", ".join(e["name"] for e in index[kind])))
        elif args.verb == "publish":
            kind = GEODATA if args.what == GEODATA else NODESETS
            tags = [t.strip() for t in (args.tags or "").split(",") if t.strip()]
            entry = asyncio.run(_with_session(lambda session: publish(
                kind, args.name, args.index, session, entry_name=args.entry, title=args.title,
                description=args.description, tags=tags,
                geodata_for=getattr(args, "geodata", None))))
            print("published %s %s: %s, %s, sha256 %s" % (
                args.what, entry["name"], entry["url"], store.human_bytes(entry["bytes"]),
                entry["sha256"]))
            print("in %s: commit and push it for the index to list it" % index_file(args.index))
        elif args.what == GEODATA:
            _geodata_cli(args)
        else:
            _nodeset_cli(args)
    except store.StoreError as err:
        print("sim %s: %s" % (args.what, err), file=sys.stderr)
        return 1
    return 0


def _geodata_cli(args):
    if args.verb == "list":
        for name in geodata_module.names():
            origin = geodata_origin(name)
            size = store.human_bytes(store.disk_bytes(geodata_module.geodata_dir(name)))
            try:
                gd = geodata_module.load(name)
                what = "pack" if gd.is_pack else "synthetic"
            except store.StoreError as err:
                what = "! %s" % err
            print("%-32s %8s  %-10s %s" % (name, size, what,
                                           "from %s" % origin["index"] if origin else ""))
    elif args.verb == "offered":
        _offered(GEODATA, args.substring)
    elif args.verb == "add":
        zips = [n for n in args.names if n.endswith(".zip") or os.sep in n]
        for path in zips:
            gd = geodata_module.import_zip(caller_path(path))
            print("added geodata %s from %s" % (gd.name, path))
        rest = [n for n in args.names if n not in zips]
        if rest:
            _install_cli(GEODATA, rest)
    else:
        for name in args.names:
            if not os.path.isfile(geodata_module.geodata_path(name)):
                raise IndexFault("no geodata called %s" % name)
        if not _confirm("geodata", args.names, args.force):
            return
        for name in args.names:
            geodata_module.delete(name)
            print("deleted geodata %s" % name)


def _nodeset_cli(args):
    if args.verb == "list":
        for name in nodeset_module.names():
            origin = nodeset_origin(name)
            size = store.human_bytes(nodeset_module.disk_bytes(name))
            try:
                nodes = "%d nodes" % len(nodeset_module.load(name).nodes)
            except store.StoreError as err:
                nodes = "! %s" % err
            came = ""
            if origin:
                came = "from %s%s" % (origin["index"], ", changed here" if origin["changed"] else "")
            print("%-32s %8s  %-10s %s" % (name, size, nodes, came))
    elif args.verb == "offered":
        _offered(NODESETS, args.substring)
    elif args.verb == "add":
        _install_cli(NODESETS, args.names)
    else:
        for name in args.names:
            if not os.path.isfile(nodeset_module.nodeset_path(name)):
                raise IndexFault("no nodeset called %s" % name)
        if not _confirm("nodesets", args.names, args.force):
            return
        for name in args.names:
            nodeset_module.delete(name)
            print("deleted nodeset %s" % name)


if __name__ == "__main__":
    sys.exit(main())
