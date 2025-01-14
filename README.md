# nameshed

A primary name server written in Rust.

## tl;dr

A quick example of NSD acting as primary, with nameshed acting as a secondary
in order to receive the zone via XFR, sign it and serve the signed zone.

$ cat /etc/nsd/zones/example.com
$ORIGIN example.com.
$TTL 86400 ; default time-to-live for this zone

example.com.   IN  SOA     ns.example.com. noc.dns.example.org. (
        2020080302  ;Serial
        7200        ;Refresh
        3600        ;Retry
        1209600     ;Expire
        3600        ;Negative response caching TTL
)

; The nameservers that are authoritative for this zone.
                                NS      example.com.

; A and AAAA records are for IPv4 and IPv6 addresses respectively
example.com.    5000    A       192.0.2.1
                        AAAA    2001:db8::3

; A CNAME redirects from www.example.com to example.com
www                             CNAME   example.com.

mail                    MX      10      example.com.

$ tail -n 10 /etc/nsd/nsd.conf
key:
  name: "sec1-key"
  algorithm: sha256
  secret: "zlCZbVJPIhobIs1gJNQfrsS3xCxxsR9pMUrGwG8OgG8="
  
zone:
    name: example.com
    zonefile: "zones/example.com"
    notify: 127.0.0.1@8054 NOKEY
    provide-xfr: 127.0.0.1 sec1-key

$ grep 8055 /etc/nsd/nsd.conf
        port: 8055

$ sudo -u nsd nsd -c /etc/nsd/nsd.conf

$ cargo run -- -c nameshed.conf --listen 127.0.0.1:8053

In another terminal:

$ dig +short @127.0.0.1 -p 8055 SOA example.com
ns.example.com. noc.dns.example.org. 2020080302 7200 3600 1209600 3600

$ dig +short @127.0.0.1 -p 8055 SOA example.com
ns.example.com. noc.dns.example.org. 2020080302 7200 3600 1209600 3600

$ dig +noall +answer @127.0.0.1 -p 8054 AXFR example.com | dnssec-verify -o example.com -x /dev/stdin 
Loading zone 'example.com' from file '/dev/stdin'

Verifying the zone using the following algorithms:
- ED25519
Zone fully signed:
Algorithm: ED25519: KSKs: 1 active, 0 stand-by, 0 revoked
                    ZSKs: 0 active, 0 present, 0 revoked