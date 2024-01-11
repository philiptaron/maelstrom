 Installing Clustered Job Runner

This covers setting up the clustered job runner. This is split into two
different parts.

- **The Broker**. This is the brains of the clustered job runner, clients and
  workers connect to it.
- **The Worker**. There are one or many of these running on the same or different
  machines from each other and the broker.

The broker and the worker only work on Linux. They both can be installed using
cargo.

## Installing the Broker

We will use cargo install the broker, but first we need to install some
dependencies. First make sure you've installed
[Rust](https://www.rust-lang.org/tools/install). Then install these other
required things.

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli wasm-opt
```

Now we can install the broker

```bash
export METICULOUS_GITHUB="https://github.com/meticulous-software/meticulous.git"
cargo install --git $METICULOUS_GITHUB meticulous-broker
```

It is best to not run the service as root, so we will create a new user to use
for this purpose. The broker also uses the local file-system to cache artifacts
so this will give us a place to put them.

```bash
sudo adduser meticulous-broker
sudo mkdir ~meticulous-broker/bin
sudo mv ~/.cargo/bin/meticulous-broker ~meticulous-broker/bin/
```

We need now to run the broker as a service. This guide will cover using
[Systemd](https://systemd.io) to do this.

Create a service file at `/etc/systemd/system/meticulous-broker.service` and
fill it with the following contents.

```language-systemd
[Unit]
Description=Meticulous Broker

[Service]
User=meticulous-broker
WorkingDirectory=/home/meticulous-broker
ExecStart=/home/meticulous-broker/bin/meticulous-broker --http-port 9000 --port 9001
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
```

Now we install and start the broker

```bash
sudo systemctl enable meticulous-broker
sudo systemctl start meticulous-broker
```

The broker should be hopefully be running now and you can confirm by using a web
browser and navigating to the web interface running on the HTTP port.

The broker listens on two different ports which we have provided to it via
command-line arguments. The HTTP port has a web interface we can use to monitor
and interact with the broker. The other port is the port workers and clients
will connect to.

It stores its caches in `<working-directory>/.cache/meticulous-broker`. For the
given set-up this should be `/home/meticulous-broker/.cache/meticulous-broker`

## Installing the Worker

You are allowed to have as many worker instances as you would like. Typically
you should install one per machine you wish to be involved in running jobs.

First make sure you've installed [Rust](https://www.rust-lang.org/tools/install).

Install the worker with

```bash
export METICULOUS_GITHUB="https://github.com/meticulous-software/meticulous.git"
cargo install --git $METICULOUS_GITHUB meticulous-worker
```

It is best to not run the service as root, so we will create a new user to use
for this purpose. The worker also uses the local file-system to cache artifacts
so this will give us a place to put them.

```bash
sudo adduser meticulous-worker
sudo mkdir ~meticulous-worker/bin
sudo mv ~/.cargo/bin/meticulous-worker ~meticulous-worker/bin/
```

We need now to run the worker as a service. This guide will cover using
[Systemd](https://systemd.io) to do this.

Create a service file at `/etc/systemd/system/meticulous-worker.service` and
fill it with the following contents.

```language-systemd
[Unit]
Description=Meticulous Worker

[Service]
User=meticulous-worker
WorkingDirectory=/home/meticulous-worker
ExecStart=/home/meticulous-worker/bin/meticulous-worker --broker <broker-machine-address>:9001
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
```

Replace `<broker-machine-address>` with the hostname or IP of the broker machine
(or localhost if its running on the same machine). 9001 is the port we chose
when setting up the broker.

Now we install and start the worker

```bash
sudo systemctl enable meticulous-worker
sudo systemctl start meticulous-worker
```

The worker should be running now. To make sure you can pull up the broker web UI
and it should now show that there is 1 worker.

It stores its caches in `<working-directory>/.cache/meticulous-worker`. For the
given set-up this should be `/home/meticulous-worker/.cache/meticulous-worker`

Repeat these steps for every machine you wish to install a worker on to.