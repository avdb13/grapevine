# Keep sorted
{ buildEnv
, coreutils
, default
, dockerTools
, envsubst
, moreutils
, openssl
, writeShellScript
, writeTextDir
}:

dockerTools.buildImage {
  name = "complement-grapevine";

  copyToRoot = buildEnv {
    name = "image-root";
    paths = [
     (writeTextDir "app/config.toml" (builtins.readFile ./config.toml))
     coreutils
     default
     moreutils
     envsubst
     openssl
   ];
    pathsToLink = [ "/bin" "/app" ];
  };

  config = {
    ExposedPorts = {
      "8008/tcp" = {};
      "8448/tcp" = {};
    };
    Cmd = [
      (writeShellScript "docker-entrypoint.sh" ''
        set -euo pipefail

        mkdir -p /tmp

        # trust certs signed by the complement test CA
        mkdir -p /etc/ca-certificates
        cp /complement/ca/ca.crt /etc/ca-certificates/
        # sign our TLS cert with the complement test CA
        openssl genrsa \
          -out /app/grapevine.key \
          2048
        openssl req -new \
          -sha256 \
          -key /app/grapevine.key \
          -subj "/CN=$SERVER_NAME" \
          -out /app/grapevine.csr
        openssl x509 -req \
          -in /app/grapevine.csr \
          -CA /complement/ca/ca.crt \
          -CAkey /complement/ca/ca.key \
          -CAcreateserial \
          -out /app/grapevine.crt \
          -days 365 \
          -sha256

        envsubst --no-unset < /app/config.toml | sponge /app/config.toml

        grapevine --config /app/config.toml
      '')
    ];
  };
}
