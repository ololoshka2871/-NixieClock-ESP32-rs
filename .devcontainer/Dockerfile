FROM rust:1.86-slim

# for ci scripts run
RUN apt update && \
    apt install -y \
	    curl \
	    binutils-xtensa-lx106 \
	    gcc-xtensa-lx106 \
	    unzip \
	    nodejs \
	    git && \
    apt clean && \
    rm -rf /var/lib/apt/lists/*

# install ldproxy
RUN mkdir -p $HOME/.cargo/bin && \
	curl -L https://github.com/esp-rs/embuild/releases/latest/download/ldproxy-x86_64-unknown-linux-gnu.zip -o $HOME/ldproxy.zip && \
	unzip $HOME/ldproxy.zip -d "$HOME/.cargo/bin" && \
	chmod a+x "$HOME/.cargo/bin/ldproxy"

# Install espup
RUN curl -L https://github.com/esp-rs/espup/releases/latest/download/espup-x86_64-unknown-linux-gnu -o "$HOME/.cargo/bin/espup" && \
	chmod a+x "$HOME/.cargo/bin/espup"

# Install esp32 toolchain
RUN "$HOME/.cargo/bin/espup" install -l debug --export-file $HOME/exports --targets esp32 && \
	. "$HOME/exports" && \
	rustup default esp

RUN	ln -s $(grep "export PATH" "$HOME/exports" | cut -d = -f 2 | sed s/\"//g | cut -d : -f 1) /usr/local/bin/esp-gcc 

ENV PATH=/usr/local/bin/esp-gcc:$PATH