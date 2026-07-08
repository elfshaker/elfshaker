use sha1::{Digest, Sha1};
use std::convert::TryInto;

struct Fixture {
    index: &'static [u8],
    pack: &'static [u8],
    version: u32,
    index_sha1: &'static str,
    pack_sha1: &'static str,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        index: include_bytes!("fixtures/legacy/linux-v1.pack.idx"),
        pack: include_bytes!("fixtures/legacy/linux-v1.pack"),
        version: 1,
        index_sha1: "fdddba26e920741df7a3a2c8877e07e19d3bfc1a",
        pack_sha1: "f7388346393a5935c4c0ecc0ab69a5f08d2f9546",
    },
    Fixture {
        index: include_bytes!("fixtures/legacy/linux-v2.pack.idx"),
        pack: include_bytes!("fixtures/legacy/linux-v2.pack"),
        version: 2,
        index_sha1: "929143057c86a240629d1f6d94b24aa28182bd2d",
        pack_sha1: "f77dbcb4ee9fb730addc57baf83d7db0804139d1",
    },
    Fixture {
        index: include_bytes!("fixtures/legacy/windows-v1.pack.idx"),
        pack: include_bytes!("fixtures/legacy/windows-v1.pack"),
        version: 1,
        index_sha1: "b5dbf4e00cb7634a00f11de99416de686be13df1",
        pack_sha1: "f391a390f2066c57b1cd74d92bf15ff6f9fb88c7",
    },
    Fixture {
        index: include_bytes!("fixtures/legacy/windows-v2.pack.idx"),
        pack: include_bytes!("fixtures/legacy/windows-v2.pack"),
        version: 2,
        index_sha1: "624f51727e7153f219d2643fbda00680ba5bc41a",
        pack_sha1: "f391a390f2066c57b1cd74d92bf15ff6f9fb88c7",
    },
];

fn sha1(bytes: &[u8]) -> String {
    hex::encode(Sha1::digest(bytes))
}

#[test]
fn legacy_fixture_headers_are_versioned() {
    for fixture in FIXTURES {
        assert_eq!(&fixture.index[..4], b"ELFS");
        assert_eq!(
            u32::from_be_bytes(fixture.index[4..8].try_into().unwrap()),
            fixture.version
        );
        assert!(!fixture.pack.is_empty());
    }
}

#[test]
fn legacy_fixture_contents_are_stable() {
    for fixture in FIXTURES {
        assert_eq!(sha1(fixture.index), fixture.index_sha1);
        assert_eq!(sha1(fixture.pack), fixture.pack_sha1);
    }
}
