# nameshed

A primary name server written in Rust.

This project is under heavy development, watch this space!

# About this branch

This branch explores using an underlying framework based on Rotonda (which in
turn was originally based on RTRTR). This provides a dynamic graph based
connected component system with direct (async fn based) "event" passing from
one component to its downstream (in the graph) components, and with indirect
(message queue) based sending of commands out of graph order from any
component to any other.

The `nameshed.conf` is correspondingly modified to define the following
components:

  - ZL: "Zone Loader": An instance of `ZoneLoader` responsible for receiving
    incoming zones via XFR.

  - RS: "Pre-Signing Review Server": An instance of `ZoneServer` responsible
    for serving an unsigned loaded zone for review.

  - ZS: "Zone Signer": An instance of `ZoneSigner` responsible for signing
    an approved unsigned loaded zone.

  - RS2: "Post-Signing Review Server": An instance of `ZoneServer` responsible
    for serving an unsigned loaded zone for review.

  - PS: "Publication Server": An instance of `ZoneServer` responsible for
    serving approved signed zones.

  - CC: "Central Command": Responsible for receiving events from all other
    components and then dispatching commands to them in order to trigger the
    next action that should occur, e.g. start serving a new copy of an
    unsigned review because it has been loaded.

ZL, RS, ZS, RS2 and PS send their events downtream to CC.

CC currently assumes it knows the names of its upstream components in order to
send commands to them by name.

Quickly tested with a local `NSD` acting as primary with `nameshed` acting as
secondary.