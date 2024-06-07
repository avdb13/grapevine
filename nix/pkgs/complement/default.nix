# Dependencies (keep sorted)
{ buildGoModule
, fetchFromGitHub
, lib
, olm
}:

buildGoModule {
  name = "complement";

  src = fetchFromGitHub {
    owner = "matrix-org";
    repo = "complement";
    rev = "8587fb3cbe746754b2c883ff6c818ca4d987d0a5";
    hash = "sha256-cie+b5N/TQAFD8vF/XbqfyFJkFU0qUPDbtJQDm/TfQc=";
  };

  vendorHash = "sha256-GyvxXUOoXnRebfdgZgTdg34/zKvWmf0igOfblho9OTc=";

  buildInputs = [ olm ];

  doCheck = false;
  postBuild = ''
    # compiles the tests into a binary
    go test -c ./tests -o "$GOPATH/bin/complement.test"
  '';

  meta = {
    description = "Matrix compliance test suite";
    homepage = "https://github.com/matrix-org/complement";
    license = lib.licenses.asl20;
  };
}
