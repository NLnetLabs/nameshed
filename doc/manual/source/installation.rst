Installation
============

Binary Packages
---------------

Getting started with Cascade is really easy by installing a binary package
for either Debian and Ubuntu or for Red Hat Enterprise Linux (RHEL) and
compatible systems such as Rocky Linux. Alternatively, you can run with
Docker. 

You can also build Cascade from the source code using Cargo, Rust's build
system and package manager. Cargo lets you to run Cascade on almost any
operating system and CPU architecture. Refer to the :doc:`building` section
to get started.

.. tabs::

   .. group-tab:: Debian

       To install a Cascade package, you need the 64-bit version of one of
       these Debian versions:

         -  Debian Trixie 13
         -  Debian Bookworm 12
         -  Debian Bullseye 11

       Packages for the ``amd64``/``x86_64`` architecture are available for
       all listed versions. In addition, we offer ``armhf`` architecture
       packages for Debian/Raspbian Bullseye, and ``arm64`` for Buster.
       
       First update the :program:`apt` package index: 

       .. code-block:: bash

          sudo apt update

       Then install packages to allow :program:`apt` to use a repository over HTTPS:

       .. code-block:: bash

          sudo apt install \
            ca-certificates \
            curl \
            gnupg \
            lsb-release

       Add the GPG key from NLnet Labs:

       .. code-block:: bash

          curl -fsSL https://packages.nlnetlabs.nl/aptkey.asc | sudo gpg --dearmor -o /usr/share/keyrings/nlnetlabs-archive-keyring.gpg

       Now, use the following command to set up the *main* repository:

       .. code-block:: bash

          echo \
          "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/nlnetlabs-archive-keyring.gpg] https://packages.nlnetlabs.nl/linux/debian \
          $(lsb_release -cs) main" | sudo tee /etc/apt/sources.list.d/nlnetlabs.list > /dev/null

       Update the :program:`apt` package index once more: 

       .. code-block:: bash

          sudo apt update

       You can now install Cascade with:

       .. code-block:: bash

          sudo apt install cascade

       After installation Cascade will run immediately as the user
       *cascade* and be configured to start at boot. 
       
       You can check the status of Cascade with:
       
       .. code-block:: bash 
       
          sudo systemctl status cascade
       
       You can view the logs with: 
       
       .. code-block:: bash
       
          sudo journalctl --unit=cascade

   .. group-tab:: Ubuntu

       To install a Cascade package, you need the 64-bit version of one of
       these Ubuntu versions:

         - Ubuntu Noble 24.04 (LTS)
         - Ubuntu Jammy 22.04 (LTS)
         - Ubuntu Focal 20.04 (LTS)

       Packages are available for the ``amd64``/``x86_64`` architecture only.
       
       First update the :program:`apt` package index: 

       .. code-block:: bash

          sudo apt update

       Then install packages to allow :program:`apt` to use a repository over HTTPS:

       .. code-block:: bash

          sudo apt install \
            ca-certificates \
            curl \
            gnupg \
            lsb-release

       Add the GPG key from NLnet Labs:

       .. code-block:: bash

          curl -fsSL https://packages.nlnetlabs.nl/aptkey.asc | sudo gpg --dearmor -o /usr/share/keyrings/nlnetlabs-archive-keyring.gpg

       Now, use the following command to set up the *main* repository:

       .. code-block:: bash

          echo \
          "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/nlnetlabs-archive-keyring.gpg] https://packages.nlnetlabs.nl/linux/ubuntu \
          $(lsb_release -cs) main" | sudo tee /etc/apt/sources.list.d/nlnetlabs.list > /dev/null

       Update the :program:`apt` package index once more: 

       .. code-block:: bash

          sudo apt update

       You can now install Cascade with:

       .. code-block:: bash

          sudo apt install cascade

       After installation Cascade will run immediately as the user
       *cascade* and be configured to start at boot. 
       
       You can check the status of Cascade with:
       
       .. code-block:: bash 
       
          sudo systemctl status cascade
       
       You can view the logs with: 
       
       .. code-block:: bash
       
          sudo journalctl --unit=Cascade

   .. group-tab:: RHEL

       To install a Cascade package, you need Red Hat Enterprise Linux
       (RHEL) 8 or 9, or compatible operating system such as Rocky Linux.
       Packages are available for the ``amd64``/``x86_64`` architecture only.
       
       First create a file named :file:`/etc/yum.repos.d/nlnetlabs.repo`,
       enter this configuration and save it:
       
       .. code-block:: text
       
          [nlnetlabs]
          name=NLnet Labs
          baseurl=https://packages.nlnetlabs.nl/linux/centos/$releasever/main/$basearch
          enabled=1
        
       Add the GPG key from NLnet Labs:
       
       .. code-block:: bash
       
          sudo rpm --import https://packages.nlnetlabs.nl/aptkey.asc
       
       You can now install Cascade with:

       .. code-block:: bash

          sudo yum install -y cascade

       After installation Cascade will run immediately as the user
       *Cascade* and be configured to start at boot. 
       
       You can check the status of Cascade with:
       
       .. code-block:: bash 
       
          sudo systemctl status cascade
       
       You can view the logs with: 
       
       .. code-block:: bash
       
          sudo journalctl --unit=cascade
       
   .. group-tab:: Docker

       Cascade Docker images are built with Alpine Linux. The supported 
       CPU architectures are shown on the `Docker Hub Cascade page 
       <https://hub.docker.com/r/nlnetlabs/Cascade/tags>`_ per Cascade
       version (aka Docker "tag") in the ``OS/ARCH`` column.

       To run Cascade as a background daemon with the default settings can be
       done like so:

       .. code-block:: bash

          sudo docker run -d --restart=unless-stopped --name cascade
          [TODO]

Updating
--------

.. tabs::

   .. group-tab:: Debian

       To update an existing Cascade installation, first update the 
       repository using:

       .. code-block:: text

          sudo apt update

       You can use this command to get an overview of the available versions:

       .. code-block:: text

          sudo apt policy cascade

       You can upgrade an existing Cascade installation to the latest
       version using:

       .. code-block:: text

          sudo apt --only-upgrade install cascade

   .. group-tab:: Ubuntu

       To update an existing Cascade installation, first update the 
       repository using:

       .. code-block:: text

          sudo apt update

       You can use this command to get an overview of the available versions:

       .. code-block:: text

          sudo apt policy cascade

       You can upgrade an existing Cascade installation to the latest
       version using:

       .. code-block:: text

          sudo apt --only-upgrade install cascade

   .. group-tab:: RHEL

       To update an existing Cascade installation, you can use this
       command to get an overview of the available versions:
        
       .. code-block:: bash
        
          sudo yum list --showduplicates cascade
          
       You can update to the latest version using:
         
       .. code-block:: bash
         
          sudo yum update -y cascade
             
   .. group-tab:: Docker

       Assuming that you run Docker with image `nlnetlabs/cascade`, upgrading
       to the latest version can be done by running the following commands:
        
       .. code-block:: text
       
          sudo docker pull nlnetlabs/cascade
          sudo docker rm --force cascade
          sudo docker run <your usual arguments> nlnetlabs/cascade

Installing Specific Versions
----------------------------

Before every new release of Cascade, one or more release candidates are 
provided for testing through every installation method. You can also install
a specific version, if needed.

.. tabs::

   .. group-tab:: Debian

       If you would like to try out release candidates of Cascade you can
       add the *proposed* repository to the existing *main* repository
       described earlier. 
       
       Assuming you already have followed the steps to install regular releases,
       run this command to add the additional repository:

       .. code-block:: bash

          echo \
          "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/nlnetlabs-archive-keyring.gpg] https://packages.nlnetlabs.nl/linux/debian \
          $(lsb_release -cs)-proposed main" | sudo tee /etc/apt/sources.list.d/nlnetlabs-proposed.list > /dev/null

       Make sure to update the :program:`apt` package index:

       .. code-block:: bash

          sudo apt update
       
       You can now use this command to get an overview of the available 
       versions:

       .. code-block:: bash

          sudo apt policy cascade

       You can install a specific version using ``<package name>=<version>``,
       e.g.:

       .. code-block:: bash

          sudo apt install Cascade=0.1.0~rc1-1bookworm

   .. group-tab:: Ubuntu

       If you would like to try out release candidates of Cascade you can
       add the *proposed* repository to the existing *main* repository
       described earlier. 
       
       Assuming you already have followed the steps to install regular
       releases, run this command to add the additional repository:

       .. code-block:: bash

          echo \
          "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/nlnetlabs-archive-keyring.gpg] https://packages.nlnetlabs.nl/linux/ubuntu \
          $(lsb_release -cs)-proposed main" | sudo tee /etc/apt/sources.list.d/nlnetlabs-proposed.list > /dev/null

       Make sure to update the :program:`apt` package index:

       .. code-block:: bash

          sudo apt update
       
       You can now use this command to get an overview of the available 
       versions:

       .. code-block:: bash

          sudo apt policy Cascade

       You can install a specific version using ``<package name>=<version>``,
       e.g.:

       .. code-block:: bash

          sudo apt install cascade=0.1.0~rc1-1jammy
          
   .. group-tab:: RHEL

       To install release candidates of Cascade, create an additional repo 
       file named :file:`/etc/yum.repos.d/nlnetlabs-testing.repo`, enter this
       configuration and save it:
       
       .. code-block:: text
       
          [nlnetlabs-testing]
          name=NLnet Labs Testing
          baseurl=https://packages.nlnetlabs.nl/linux/centos/$releasever/proposed/$basearch
          enabled=1
        
       You can use this command to get an overview of the available versions:
        
       .. code-block:: bash
        
          sudo yum list --showduplicates cascade
          
       You can install a specific version using 
       ``<package name>-<version info>``, e.g.:
         
       .. code-block:: bash
         
          sudo yum install -y cascade-0.1.0~rc1
             
   .. group-tab:: Docker

       All release versions of Cascade, as well as release candidates and
       builds based on the latest main branch are available on `Docker Hub
       <https://hub.docker.com/r/nlnetlabs/Cascade/tags?page=1&ordering=last_updated>`_. 
       
       For example, installing Cascade 0.1.0 RC1 is as simple as:
        
       .. code-block:: text
       
          sudo docker run <your usual arguments> nlnetlabs/cascade:v0.1.0-rc1
               
