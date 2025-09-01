Building From Source
====================

In addition to meeting the :ref:`system requirements <installation:System
Requirements>`, there are two things you need to build Cascade: a C toolchain
and Rust. You can run Cascade on any operating system and CPU architecture
where you can fulfil these requirements.

Dependencies
------------

To get started you need a C toolchain  because of the cryptographic
primitives used by Cascade require it. You also need Rust because that’s the
programming language that Cascade has been written in.

C Toolchain
"""""""""""

Some of the libraries Cascade depends on require a C toolchain to be
present. Your system probably has some easy way to install the minimum set of
packages to build from C sources. For example, this command will install
everything you need on Debian/Ubuntu:

.. code-block:: text

  apt install build-essential

If you are unsure, try to run :command:`cc` on a command line. If there is a
complaint about missing input files, you are probably good to go.

Rust
""""

The Rust compiler runs on, and compiles to, a great number of platforms,
though not all of them are equally supported. The official `Rust Platform
Support`_ page provides an overview of the various support levels.

While some system distributions include Rust as system packages, Cascade
relies on a relatively new version of Rust, currently |rustversion| or newer.
We therefore suggest to use the canonical Rust installation via a tool called
:program:`rustup`.

Assuming you already have :program:`curl` installed, you can install
:program:`rustup` and Rust by simply entering:

.. code-block:: text

  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

Alternatively, visit the `Rust website
<https://www.rust-lang.org/tools/install>`_ for other installation methods.

Building and Updating
---------------------

In Rust, a library or executable program such as Cascade is called a *crate*.
Crates are published on `crates.io <https://crates.io/>`_, the Rust package
registry. Cargo is the Rust package manager. It is a tool that allows Rust
packages to declare their various dependencies and ensure that you’ll always
get a repeatable build. 

Cargo fetches and builds Cascade’s dependencies into an executable binary
for your platform. By default you install from crates.io, but you can for
example also install from a specific Git URL, as explained below.

Installing the latest Cascade release from crates.io is as simple as
running:

.. code-block:: text

  cargo install --locked cascade

The command will build Cascade and install it in the same directory that
Cargo itself lives in, likely ``$HOME/.cargo/bin``. This means Cascade
will be in your path, too.

Updating
""""""""

If you want to update to the latest version of Cascade, it’s recommended
to update Rust itself as well, using:

.. code-block:: text

    rustup update

Use the ``--force`` option to overwrite an existing version with the latest
Cascade release:

.. code-block:: text

    cargo install --locked --force cascade

Installing Specific Versions
""""""""""""""""""""""""""""

If you want to install a specific version of
Cascade using Cargo, explicitly use the ``--version`` option. If needed,
use the ``--force`` option to overwrite an existing version:
        
.. code-block:: text

    cargo install --locked --force cascade --version 0.1.0-rc1

All new features of Cascade are built on a branch and merged via a `pull
request <https://github.com/NLnetLabs/Cascade/pulls>`_, allowing you to
easily try them out using Cargo. If you want to try a specific branch from
the repository you can use the ``--git`` and ``--branch`` options:

.. code-block:: text

    cargo install --git https://github.com/NLnetLabs/cascade.git --branch main
    
.. Seealso:: For more installation options refer to the `Cargo book
             <https://doc.rust-lang.org/cargo/commands/cargo-install.html#install-options>`_.

Statically Linked Cascade
-------------------------

While Rust binaries are mostly statically linked, they depend on
:program:`libc` which, as least as :program:`glibc` that is standard on Linux
systems, is somewhat difficult to link statically. This is why Cascade
binaries are actually dynamically linked on :program:`glibc` systems and can
only be transferred between systems with the same :program:`glibc` versions.

However, Rust can build binaries based on the alternative implementation
named :program:`musl` that can easily be statically linked. Building such
binaries is easy with :program:`rustup`. You need to install :program:`musl`
and the correct :program:`musl` target such as ``x86_64-unknown-linux-musl``
for x86\_64 Linux systems. Then you can just build Cascade for that
target.

On a Debian (and presumably Ubuntu) system, enter the following:

.. code-block:: bash

   sudo apt-get install musl-tools
   rustup target add x86_64-unknown-linux-musl
   cargo build --target=x86_64-unknown-linux-musl --release

Platform Specific Instructions
------------------------------

For some platforms, :program:`rustup` cannot provide binary releases to
install directly. The `Rust Platform Support`_ page lists
several platforms where official binary releases are not available, but Rust
is still guaranteed to build. For these platforms, automated tests are not
run so it’s not guaranteed to produce a working build, but they often work to
quite a good degree.

.. _Rust Platform Support:  https://doc.rust-lang.org/nightly/rustc/platform-support.html

OpenBSD
"""""""

On OpenBSD, `patches
<https://github.com/openbsd/ports/tree/master/lang/rust/patches>`_ are
required to get Rust running correctly, but these are well maintained and
offer the latest version of Rust quite quickly.

Rust can be installed on OpenBSD by running:

.. code-block:: bash

   pkg_add rust
