https://docs.redhat.com/en/documentation/red_hat_enterprise_linux/6/html/deployment_guide/sec-yum_repository

$ mkdir -p pkg/rpm/repo/4/local/i386/RPMS
$ yumdownloader dnst
$ mv dnst-0.1.0~rc1-1.x86_64.rpm pkg/rpm/repo/4/local/i386/RPMS/
$ createrepo --database pkg/rpm/repo/4/local/i386
$ chmod -R o-w+r pkg/rpm/repo/4/local
