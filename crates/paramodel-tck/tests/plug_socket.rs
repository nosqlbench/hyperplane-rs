// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! SRD-0005 plug/socket facet-compatibility coverage.

use std::collections::BTreeSet;

use paramodel_elements::{Facet, FacetKey, FacetValue, Plug, PortName, Socket};

fn pn(s: &str) -> PortName {
    PortName::new(s).unwrap()
}
fn facet(k: &str, v: &str) -> Facet {
    Facet {
        key:   FacetKey::new(k).unwrap(),
        value: FacetValue::new(v).unwrap(),
    }
}

fn facets(pairs: &[(&str, &str)]) -> BTreeSet<Facet> {
    pairs.iter().map(|(k, v)| facet(k, v)).collect()
}

// ---------------------------------------------------------------------------
// Plug/Socket construction.
// ---------------------------------------------------------------------------

#[test]
fn plug_with_empty_facet_set_is_rejected() {
    let err = Plug::new(pn("p"), BTreeSet::new()).unwrap_err();
    let _ = err;
}

#[test]
fn socket_with_empty_facet_set_is_rejected() {
    let err = Socket::new(pn("s"), BTreeSet::new()).unwrap_err();
    let _ = err;
}

// ---------------------------------------------------------------------------
// fits(): superset compatibility.
// ---------------------------------------------------------------------------

#[test]
fn plug_fits_socket_when_socket_supersets_facets() {
    let plug = Plug::new(pn("p"), facets(&[("protocol", "tcp")])).unwrap();
    let socket = Socket::new(
        pn("s"),
        facets(&[("protocol", "tcp"), ("tls", "any")]),
    )
    .unwrap();
    assert!(plug.fits(&socket));
    assert!(paramodel_elements::fits(&plug, &socket));
}

#[test]
fn plug_does_not_fit_socket_missing_required_facet() {
    let plug = Plug::new(
        pn("p"),
        facets(&[("protocol", "tcp"), ("auth", "mtls")]),
    )
    .unwrap();
    let socket = Socket::new(pn("s"), facets(&[("protocol", "tcp")])).unwrap();
    assert!(!plug.fits(&socket));
}

#[test]
fn plug_does_not_fit_socket_with_mismatched_value() {
    // Facet values are part of the compatibility key — `protocol=tcp`
    // does NOT match `protocol=udp`.
    let plug = Plug::new(pn("p"), facets(&[("protocol", "tcp")])).unwrap();
    let socket = Socket::new(pn("s"), facets(&[("protocol", "udp")])).unwrap();
    assert!(!plug.fits(&socket));
}

#[test]
fn plug_fits_socket_with_identical_facets() {
    let f = facets(&[("protocol", "tcp")]);
    let plug = Plug::new(pn("p"), f.clone()).unwrap();
    let socket = Socket::new(pn("s"), f).unwrap();
    assert!(plug.fits(&socket));
}

#[test]
fn socket_facets_may_extend_without_breaking_fit() {
    // Socket has everything the plug wants plus more — fits.
    let plug = Plug::new(pn("p"), facets(&[("kind", "db")])).unwrap();
    let socket = Socket::new(
        pn("s"),
        facets(&[("kind", "db"), ("tier", "prod"), ("region", "us-east")]),
    )
    .unwrap();
    assert!(plug.fits(&socket));
}
