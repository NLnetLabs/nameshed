https://wiki.centos.org/HowTos(2f)CreateLocalRepos.html

$ mkdir -p pkg/rpm/repo/10/local/x86_64/RPMS
$ yumdownloader dnst
$ mv dnst-0.1.0~rc1-1.x86_64.rpm pkg/rpm/repo/10/local/x86_64/RPMS/
$ createrepo --database pkg/rpm/repo/10/local/x86_64
$ chmod -R o-w+r pkg/rpm/repo/10/local
