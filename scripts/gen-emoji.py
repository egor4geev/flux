#!/usr/bin/env python3
"""The emoji table of Flux (`crates/flux-app/src/emoji/data.rs`, FluxGram part 4): Unicode's emoji by
category in the standard's order, with their English and Russian names and keywords for the search
and their skin tones. Run it again for a new version of the data (bump the pins below):

    python3 scripts/gen-emoji.py crates/flux-app/src/emoji/data.rs [cache folder]

Sources, pinned and checked by SHA-256 (downloaded with curl into the cache folder, a temporary one
by default):
- Unicode's `emoji-test.txt` (Emoji 15.1): the emoji in order, their groups, their skin-tone
  variants. Only the fully-qualified ones; the group "Component" (bare tones, hair) is left out.
- CLDR's annotations (`unicode-org/cldr-json`, release 44.1.0) in English and Russian: a name
  (`tts`) and keywords (`default`); the derived annotations name what the plain ones don't (flags,
  keycaps, sequences).

The table: each emoji's text, its English and Russian names, its words for the search (both
languages' names, then their keywords, lowercased, "ё" as "е", deduplicated) with the count of the
names' ones, and the index of its skin tones in a table of five variants (light → dark) for those
that take them."""
import hashlib, json, os, subprocess, sys, tempfile

EMOJI_VERSION = "15.1"
CLDR_RELEASE = "44.1.0"
CLDR = f"https://raw.githubusercontent.com/unicode-org/cldr-json/{CLDR_RELEASE}/cldr-json"
SOURCES = {
    "emoji-test.txt": (
        f"https://unicode.org/Public/emoji/{EMOJI_VERSION}/emoji-test.txt",
        "d876ee249aa28eaa76cfa6dfaa702847a8d13b062aa488d465d0395ee8137ed9",
    ),
    "en.json": (
        f"{CLDR}/cldr-annotations-full/annotations/en/annotations.json",
        "bfd8b9556a286dee0566f43f2f09c118c3080eae3b4386331528b08fbef119c2",
    ),
    "ru.json": (
        f"{CLDR}/cldr-annotations-full/annotations/ru/annotations.json",
        "b0d588a61b6a0cdc657ffc7daa8a457b41db24d94d03e3d701f8f01e902ef548",
    ),
    "en-derived.json": (
        f"{CLDR}/cldr-annotations-derived-full/annotationsDerived/en/annotations.json",
        "df3d0ab1060bca116be8a9f7592942d8f0ffe09f0267074bdaefc1bac9bf9811",
    ),
    "ru-derived.json": (
        f"{CLDR}/cldr-annotations-derived-full/annotationsDerived/ru/annotations.json",
        "3c6e814c4e2986d234b62400647070d468d21ba4475d854c7aedd149595a9f04",
    ),
}

# emoji-test's groups → the panel's categories (a name for this script, the English title, the
# strip's glyph), in order.
CATEGORIES = [
    ("people", "Smileys & People", "😀", ["Smileys & Emotion", "People & Body"]),
    ("nature", "Animals & Nature", "🐻", ["Animals & Nature"]),
    ("food", "Food & Drink", "🍔", ["Food & Drink"]),
    ("activity", "Activity", "⚽", ["Activities"]),
    ("travel", "Travel & Places", "🚗", ["Travel & Places"]),
    ("objects", "Objects", "💡", ["Objects"]),
    ("symbols", "Symbols", "🔣", ["Symbols"]),
    ("flags", "Flags", "🏁", ["Flags"]),
]
SKIPPED_GROUPS = {"Component"}
TONES = [0x1F3FB, 0x1F3FC, 0x1F3FD, 0x1F3FE, 0x1F3FF]
VS16 = 0xFE0F


def fetch(cache):
    """The sources in `cache`, downloaded if missing, each checked against its SHA-256."""
    paths = {}
    for name, (url, sha) in SOURCES.items():
        path = os.path.join(cache, name)
        if not os.path.exists(path):
            subprocess.run(["curl", "-fsSL", "-o", path, url], check=True)
        with open(path, "rb") as file:
            digest = hashlib.sha256(file.read()).hexdigest()
        if digest != sha:
            sys.exit(f"{name}: SHA-256 {digest}, expected {sha} ({url})")
        paths[name] = path
    return paths


def parse_test(path):
    """emoji-test.txt → [(group, code points, English name)], fully-qualified only, in order."""
    group, rows = None, []
    with open(path, encoding="utf-8") as file:
        for line in file:
            line = line.rstrip("\n")
            if line.startswith("# group:"):
                group = line.split(":", 1)[1].strip()
                continue
            if not line or line.startswith("#"):
                continue
            fields, _, comment = line.partition("#")
            codes, status = (part.strip() for part in fields.split(";"))
            if status != "fully-qualified" or group in SKIPPED_GROUPS:
                continue
            points = [int(code, 16) for code in codes.split()]
            # "# 😀 E1.0 grinning face": the name after the version.
            name = comment.strip().split(" ", 2)[2]
            rows.append((group, points, name))
    return rows


def annotations(path, key):
    with open(path, encoding="utf-8") as file:
        return json.load(file)[key]["annotations"]


def text(points):
    return "".join(chr(point) for point in points)


def words(phrases):
    """Lowercased words of the phrases, "ё" as "е", in order, without repeats."""
    seen, out = set(), []
    for phrase in phrases:
        for word in phrase.lower().replace("ё", "е").replace(":", " ").replace(",", " ").split():
            word = word.strip("“”\"«»()")
            if word and word not in seen:
                seen.add(word)
                out.append(word)
    return out


def rust(value):
    return json.dumps(value, ensure_ascii=False)


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    out = sys.argv[1]
    cache = sys.argv[2] if len(sys.argv) > 2 else tempfile.mkdtemp(prefix="flux-emoji-")
    os.makedirs(cache, exist_ok=True)
    paths = fetch(cache)
    rows = parse_test(paths["emoji-test.txt"])
    en = annotations(paths["en.json"], "annotations")
    ru = annotations(paths["ru.json"], "annotations")
    en_derived = annotations(paths["en-derived.json"], "annotationsDerived")
    ru_derived = annotations(paths["ru-derived.json"], "annotationsDerived")

    def lookup(table, derived, points):
        # CLDR's keys go without the variation selector mostly; some keep it.
        for key in (text([p for p in points if p != VS16]), text(points)):
            for source in (table, derived):
                if key in source:
                    return source[key]
        return {}

    # A toned variant belongs to the emoji it is with the tones (and the selectors) taken out; one
    # tone throughout (both people of a couple alike) is the tone's variant.
    bare = lambda points: tuple(p for p in points if p not in TONES and p != VS16)
    variants = {}
    for _, points, _ in rows:
        tones = {p for p in points if p in TONES}
        if len(tones) == 1:
            variants.setdefault(bare(points), {})[TONES.index(tones.pop())] = text(points)
    group_of = {group: index for index, (*_, groups) in enumerate(CATEGORIES) for group in groups}
    by_category = [[] for _ in CATEGORIES]
    tone_rows, missing = [], 0
    for group, points, name in rows:
        if any(p in TONES for p in points):
            continue
        en_entry, ru_entry = lookup(en, en_derived, points), lookup(ru, ru_derived, points)
        name_en = (en_entry.get("tts") or [name])[0]
        name_ru = (ru_entry.get("tts") or [""])[0]
        if not name_ru:
            missing += 1
        # The names' words first: a match there ranks higher.
        named = words([name_en, name_ru])
        found = words([name_en, name_ru] + en_entry.get("default", []) + ru_entry.get("default", []))
        tone = None
        found_variants = variants.get(bare(points))
        if found_variants and len(found_variants) == 5:
            tone = len(tone_rows)
            tone_rows.append([found_variants[index] for index in range(5)])
        by_category[group_of[group]].append(
            (text(points), name_en, name_ru, " ".join(found), len(named), tone)
        )

    lines = [
        "// @generated by scripts/gen-emoji.py — don't edit by hand (one line an emoji: rustfmt skips the",
        "// tables).",
        f"// Unicode emoji-test.txt {EMOJI_VERSION}; CLDR annotations (cldr-json {CLDR_RELEASE}), en and ru.",
        "",
        "use super::{Category, Emoji};",
        "",
    ]
    lines.append("#[rustfmt::skip]")
    lines.append("pub(super) static CATEGORIES: &[Category] = &[")
    start = 0
    for (key, title, icon, _), items in zip(CATEGORIES, by_category):
        end = start + len(items)
        lines.append(
            f"    Category {{ title: {rust(title)}, icon: {rust(icon)}, start: {start}, end: {end} }},"
        )
        start = end
    lines.append("];")
    lines.append("")
    lines.append("#[rustfmt::skip]")
    lines.append("pub(super) static EMOJI: &[Emoji] = &[")
    for items in by_category:
        for emoji, name_en, name_ru, found, named, tone in items:
            tone = "None" if tone is None else f"Some({tone})"
            lines.append(
                f"    Emoji {{ text: {rust(emoji)}, name: {rust(name_en)}, name_ru: {rust(name_ru)}, "
                f"words: {rust(found)}, name_words: {named}, tones: {tone} }},"
            )
    lines.append("];")
    lines.append("")
    lines.append("#[rustfmt::skip]")
    lines.append("pub(super) static TONES: &[[&str; 5]] = &[")
    for row in tone_rows:
        lines.append("    [" + ", ".join(rust(variant) for variant in row) + "],")
    lines.append("];")
    with open(out, "w", encoding="utf-8") as file:
        file.write("\n".join(lines) + "\n")
    total = sum(len(items) for items in by_category)
    print(f"{out}: {total} emoji, {len(tone_rows)} with skin tones, {missing} without a Russian name")


if __name__ == "__main__":
    main()
