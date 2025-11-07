## Bitcoin Cap'n Proto Rust Client

This project auto-generates the client code to interact with Bitcoin Core in Rust using interprocess communication. To build the code locally, you will need to have the [`capnp`](https://capnproto.org/install.html) compiler installed on your system.

## Development Setup

### Prerequisites

To build this crate, or use it as a dependency on your own crate, you need the Cap'n Proto compiler installed on your system.

#### macOS

```bash
brew install capnp
```

#### Ubuntu/Debian

```bash
sudo apt-get install capnproto libcapnp-dev
```

## Minimum Standard Rust Version

To compile this crate your project must use a Rust compiler of **1.85** or higher.

## License

Creative Commons 1.0 Universal
