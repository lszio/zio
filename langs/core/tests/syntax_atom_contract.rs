//! Reader-supported scalar atoms must survive the source-preserving syntax boundary.
use zio_core::{
    sexp::Sexp,
    span::SourceMap,
    syntax::{SyntaxNode, inspect_source},
};

#[test]
fn reader_supported_nonfinite_numbers_and_empty_keyword_round_trip() {
    let map = SourceMap::new();
    for source in ["inf", "-inf", "NaN", ":"] {
        let original = inspect_source(&map, "atoms.zio", source).unwrap().remove(0);
        let rebuilt = SyntaxNode::from_value(&original.to_value()).unwrap_or_else(|| {
            panic!("reader-supported atom rejected by syntax boundary: {source}")
        });
        assert_eq!(rebuilt.span, original.span);
        match (original.to_sexp().unwrap(), rebuilt.to_sexp().unwrap()) {
            (Sexp::Float(a, _), Sexp::Float(b, _)) if a.is_nan() => assert!(b.is_nan()),
            (a, b) => assert_eq!(a, b, "{source}"),
        }
    }
}

#[test]
fn source_span_validation_rejects_forged_unicode_boundaries_and_locations() {
    use zio_core::span::{BytePos, SourceId, Span};
    let map = SourceMap::new();
    let id = map.register("unicode.zio".into(), "中\nx".into());
    for (start, end, line, col) in [(0, 3, 1, 1), (4, 5, 2, 1), (5, 5, 2, 2)] {
        assert!(map.valid_span(Span::new(id, BytePos(start), BytePos(end), line, col)));
    }
    for (start, end, line, col) in [
        (1, 3, 1, 2),
        (0, 2, 1, 1),
        (4, 6, 2, 1),
        (5, 4, 2, 2),
        (4, 5, 1, 5),
        (4, 5, 2, 2),
    ] {
        assert!(!map.valid_span(Span::new(id, BytePos(start), BytePos(end), line, col)));
    }
    assert!(!map.valid_span(Span::new(SourceId(0), BytePos(0), BytePos(0), 1, 1)));
    assert!(!map.valid_span(Span::new(
        SourceId(usize::MAX),
        BytePos(0),
        BytePos(0),
        1,
        1
    )));
}
