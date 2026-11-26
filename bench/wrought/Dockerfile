# The Wrought engine as built by bench/run.py (`cargo build --release
# --features app`), on the same Ubuntu release as the build host.
FROM ubuntu:resolute-20261024.1
COPY engine /usr/local/bin/engine
WORKDIR /site
EXPOSE 4400
CMD ["engine", "serve", "--site", "/site", "--host", "0.0.0.0", "--port", "4400"]
