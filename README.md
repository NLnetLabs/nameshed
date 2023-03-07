# nameshed
A primary name server written in Rust.

## tl;dr

$ cat /etc/nsd/example.com.zone 
$ORIGIN example.com.

@ IN 2000 SOA ns.example.com. some\.bloke.example.com. 2 86400 3500 3212 22

@ 3000 IN A 127.0.0.1
@ 3600 IN A 127.0.0.2

$ nameshed --init -d /tmp/data --listen 127.0.0.1:8053

