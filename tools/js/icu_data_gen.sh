#!/bin/sh
# Make crates/js/src/icu_data: the ICU4X data of bare-js, for the root locale and 5 English locales (en, en-US, en-GB, en-CA, en-IN).
#   cargo install icu4x-datagen --version 2.4.0
#   tools/js/icu_data_gen.sh
# It downloads CLDR 49 (about 80 MB, kept in $WORK) and needs the disk, not /tmp: /tmp can be a small tmpfs.
# Markers: the decimal and plural rules, the root collation and the NFD tables that the collator needs.
# The NFKD tables and the NFC composition table are for `icu_normalizer` NFKC and NFKD (`eval_string_more.rs`).
# The collation tailorings are all Chinese scripts or other languages: English sorts with the root collation. The attribute
# filter for the tailoring data drops them all (an empty marker, 2.8 KB instead of 2.1 MB). The other collation markers keep
# their data (a collator reads the metadata of the locale `und`).
set -eu
here=$(dirname "$0")
out=$here/../../crates/js/src/icu_data
work=${WORK:-$HOME/.cache/bare-icu-data}
mkdir -p "$work"
markers="CollationRootV1 CollationJamoV1 CollationSpecialPrimariesV1 CollationDiacriticsV1 CollationReorderingV1 CollationMetadataV1 NormalizerNfdDataV1 NormalizerNfdTablesV1 NormalizerNfkdDataV1 NormalizerNfkdTablesV1 NormalizerNfcV1 DecimalSymbolsV1 DecimalDigitsV1 PluralsCardinalV1 PluralsOrdinalV1"
gen() {
  # shellcheck disable=SC2086
  TMPDIR=$work XDG_CACHE_HOME=$work icu4x-datagen --format baked --use-separate-crates -W \
    --locales en en-US en-GB en-CA en-IN und "$@"
}
gen --markers $markers --attribute-filter 'collator=-/.+/' --out "$work/main"
gen --markers CollationTailoringV1 --attribute-filter 'collator=-/.*/' --out "$work/tailoring"
rm -rf "$out"
cp -r "$work/main" "$out"
# mod.rs lists the tailoring file; the main run did not make it, so take mod.rs of a run with all markers
gen --markers $markers CollationTailoringV1 --attribute-filter 'collator=-/.+/' --out "$work/all"
cp "$work/all/mod.rs" "$out/mod.rs"
cp "$work/tailoring/collation_tailoring_v1.rs.data" "$out/collation_tailoring_v1.rs.data"
echo "icu_data: $(du -sh "$out" | cut -f1)"
