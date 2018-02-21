# EWF Tobalaba Client

![EWF](http://energyweb.org/wp-content/uploads/2017/02/EnergyWebnoback-1.png)

Repository for the [**Energy Web Foundation**](http://energyweb.org/) client for the Tobalaba test network.

Download this repository and compile from source or get the binaries.

## Binaries

#### [Linux](https://tobalaba.slock.it/download/ewf-client-linux)

#### [Windows](https://tobalaba.slock.it/download/ewf-client-windows.exe)

#### [Mac](https://tobalaba.slock.it/download/ewf-client-mac.zip)


## Build dependencies

**Parity requires Rust version 1.21.0 to build**

We recommend installing Rust through [rustup](https://www.rustup.rs/). If you don't already have rustup, you can install it like this:

- Linux:
	```bash
	$ curl https://sh.rustup.rs -sSf | sh
	```

	Parity also requires `gcc`, `g++`, `libssl-dev`/`openssl`, `libudev-dev` and `pkg-config` packages to be installed.

- OSX:
	```bash
	$ curl https://sh.rustup.rs -sSf | sh
	```

	`clang` is required. It comes with Xcode command line tools or can be installed with homebrew.

- Windows
  Make sure you have Visual Studio 2015 with C++ support installed. Next, download and run the rustup installer from
	https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe, start "VS2015 x64 Native Tools Command Prompt", and use the following command to install and set up the msvc toolchain:
  ```bash
	$ rustup default stable-x86_64-pc-windows-msvc
  ```

Once you have rustup, install parity or download and build from source

----

## Build from source

```bash
# download energyweb-client code
$ git clone https://github.com/energywebfoundation/energyweb-client
$ cd energyweb-client

# build in release mode
$ cargo build --release --no-default-features --features ui
```

This will produce an executable in the `./target/release` subdirectory.
Note: if cargo fails to parse manifest try:

```bash
$ ~/.cargo/bin/cargo build --release
```

> In case building fails due to `package-lock.json` sha-1 integrity check, please delete all `package-lock.json` files. They are present in root repo folder, `js` and `js-old`.

----

## Start Parity

### Manually

To start Parity manually, just run

```bash
$ ./target/release/parity --chain tobalaba
```

and Parity will begin syncing the Tobalaba blockchain.

