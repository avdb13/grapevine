use frunk::{hlist, HList};

use super::super::{FromBytes, ToBytes};

#[test]
pub(crate) fn serialize_hlist_0() {
    let expected: &[u8] = &[];

    let actual = hlist![];
    let actual_bytes = actual.to_bytes();

    assert_eq!(expected, actual_bytes.as_ref());
}

#[test]
pub(crate) fn serialize_hlist_1() {
    let expected =
        [b"hello"].into_iter().flatten().copied().collect::<Vec<_>>();

    let actual = hlist!["hello".to_owned()];
    let actual_bytes = actual.to_bytes();

    assert_eq!(expected.as_slice(), actual_bytes.as_ref());
}

#[test]
pub(crate) fn serialize_hlist_2() {
    let expected = [b"hello", [0xFF].as_slice(), b"world"]
        .into_iter()
        .flatten()
        .copied()
        .collect::<Vec<_>>();

    let actual = hlist!["hello".to_owned(), "world".to_owned()];
    let actual_bytes = actual.to_bytes();

    assert_eq!(expected.as_slice(), actual_bytes.as_ref());
}

#[test]
pub(crate) fn serialize_hlist_3() {
    let expected =
        [b"what's", [0xFF].as_slice(), b"up", [0xFF].as_slice(), b"world"]
            .into_iter()
            .flatten()
            .copied()
            .collect::<Vec<_>>();

    let actual =
        hlist!["what's".to_owned(), "up".to_owned(), "world".to_owned()];
    let actual_bytes = actual.to_bytes();

    assert_eq!(expected.as_slice(), actual_bytes.as_ref());
}

#[test]
pub(crate) fn deserialize_hlist_0() {
    let actual = <HList![]>::from_bytes(Vec::new())
        .expect("should be able to deserialize");

    assert_eq!(hlist![], actual);
}

#[test]
pub(crate) fn deserialize_hlist_1() {
    let serialized =
        [b"hello"].into_iter().flatten().copied().collect::<Vec<_>>();

    let actual = <HList![String]>::from_bytes(serialized)
        .expect("should be able to deserialize");

    assert_eq!(hlist!["hello".to_owned()], actual);
}

#[test]
pub(crate) fn deserialize_hlist_2() {
    let serialized = [b"hello", [0xFF].as_slice(), b"world"]
        .into_iter()
        .flatten()
        .copied()
        .collect::<Vec<_>>();

    let actual = <HList![String, String]>::from_bytes(serialized)
        .expect("should be able to deserialize");

    assert_eq!(hlist!["hello".to_owned(), "world".to_owned()], actual);
}

#[test]
pub(crate) fn deserialize_hlist_3() {
    let serialized =
        [b"what's", [0xFF].as_slice(), b"up", [0xFF].as_slice(), b"world"]
            .into_iter()
            .flatten()
            .copied()
            .collect::<Vec<_>>();

    let actual = <HList![String, String, String]>::from_bytes(serialized)
        .expect("should be able to deserialize");

    assert_eq!(
        hlist!["what's".to_owned(), "up".to_owned(), "world".to_owned()],
        actual
    );
}
