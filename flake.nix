{
  description = "Live multiplayer quiz for NixCon";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = fn: nixpkgs.lib.genAttrs systems (system: fn nixpkgs.legacyPackages.${system});

      # One source of truth for the version: the [package] table in Cargo.toml.
      version = builtins.head (
        builtins.match ".*\nversion = \"([^\"]+)\".*" (builtins.readFile ./Cargo.toml)
      );

      mkQuiz =
        pkgs:
        pkgs.rustPlatform.buildRustPackage {
          pname = "nixcon-quiz";
          inherit version;
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
              ./static
              ./examples
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;
          # The real questions stay out of the repository so nobody can read
          # the answers before playing; the examples show the format.
          postInstall = ''
            mkdir -p $out/share/nixcon-quiz
            cp -r examples/questions $out/share/nixcon-quiz/example-questions
          '';
          meta = {
            description = "Live multiplayer quiz for NixCon";
            mainProgram = "nixcon-quiz";
            license = pkgs.lib.licenses.mit;
            platforms = pkgs.lib.platforms.all;
          };
        };

      quizModule =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        let
          cfg = config.services.nixcon-quiz;
        in
        {
          options.services.nixcon-quiz = {
            enable = lib.mkEnableOption "the NixCon quiz server";
            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.stdenv.hostPlatform.system}.nixcon-quiz;
              defaultText = lib.literalExpression "nixcon-quiz";
              description = "Package providing the quiz server.";
            };
            questions = lib.mkOption {
              type = lib.types.path;
              default = "/var/lib/nixcon-quiz/questions";
              description = ''
                Directory with one TOML file per question. Only the server
                reads it; players never get to see it. The default keeps the
                questions out of the world-readable Nix store: copy them there
                after deploying and restart the service. Until the directory
                has files in it the service is skipped rather than failed.
              '';
            };
            title = lib.mkOption {
              type = lib.types.str;
              default = "NixCon Quiz";
              description = "Shown in the page header and the browser tab.";
            };
            questionSeconds = lib.mkOption {
              type = lib.types.ints.positive;
              default = 20;
              description = "Time to answer, the same for every question.";
            };
            revealSeconds = lib.mkOption {
              type = lib.types.ints.positive;
              default = 8;
              description = "How long the right answer stays on screen.";
            };
            roundQuestions = lib.mkOption {
              type = lib.types.ints.positive;
              default = 10;
              description = "Questions per round; after the last one the leaderboard is shown and all points reset.";
            };
            leaderboardSeconds = lib.mkOption {
              type = lib.types.ints.positive;
              default = 30;
              description = "How long the leaderboard is shown before the next round.";
            };
            host = lib.mkOption {
              type = lib.types.str;
              default = "127.0.0.1";
              description = "Address to bind.";
            };
            port = lib.mkOption {
              type = lib.types.port;
              default = 8093;
              description = "TCP port to listen on.";
            };
            domain = lib.mkOption {
              type = lib.types.nullOr lib.types.str;
              default = null;
              example = "quiz.lassul.us";
              description = ''
                When set, serve the quiz on this name through nginx with a
                Let's Encrypt certificate.
              '';
            };
            publicUrl = lib.mkOption {
              type = lib.types.nullOr lib.types.str;
              default = if cfg.domain != null then "https://${cfg.domain}/" else null;
              defaultText = lib.literalExpression ''if domain != null then "https://''${domain}/" else null'';
              description = ''
                Join address shown with a QR code on the /spectate screen. When
                null, the screen uses the host it was loaded from.
              '';
            };
          };

          config = lib.mkIf cfg.enable {
            systemd.services.nixcon-quiz = {
              description = "NixCon quiz";
              wantedBy = [ "multi-user.target" ];
              after = [ "network.target" ];
              # A fresh machine has no questions yet; don't fail the deploy.
              unitConfig.ConditionDirectoryNotEmpty = "${cfg.questions}";
              serviceConfig = {
                ExecStartPre = "${lib.getExe cfg.package} --check ${cfg.questions}";
                ExecStart =
                  lib.escapeShellArgs [
                    (lib.getExe cfg.package)
                    "--questions"
                    "${cfg.questions}"
                    "--listen"
                    "${cfg.host}:${toString cfg.port}"
                    "--title"
                    cfg.title
                    "--question-seconds"
                    cfg.questionSeconds
                    "--reveal-seconds"
                    cfg.revealSeconds
                    "--round-questions"
                    cfg.roundQuestions
                    "--leaderboard-seconds"
                    cfg.leaderboardSeconds
                  ]
                  + lib.optionalString (cfg.publicUrl != null) " --public-url ${lib.escapeShellArg cfg.publicUrl}";
                Restart = "on-failure";
                # One open event stream per player.
                LimitNOFILE = 65536;
                DynamicUser = true;
                NoNewPrivileges = true;
                PrivateDevices = true;
                PrivateTmp = true;
                ProtectHome = true;
                ProtectSystem = "strict";
                ProtectKernelTunables = true;
                ProtectControlGroups = true;
                RestrictAddressFamilies = [
                  "AF_INET"
                  "AF_INET6"
                ];
                SystemCallFilter = [ "@system-service" ];
              };
            };

            services.nginx = lib.mkIf (cfg.domain != null) {
              enable = true;
              recommendedProxySettings = lib.mkDefault true;
              virtualHosts.${cfg.domain} = {
                enableACME = lib.mkDefault true;
                forceSSL = lib.mkDefault true;
                locations."/".proxyPass = "http://${cfg.host}:${toString cfg.port}";
                locations."/api/events" = {
                  proxyPass = "http://${cfg.host}:${toString cfg.port}";
                  # Event streams stay open for the whole game.
                  extraConfig = ''
                    proxy_buffering off;
                    proxy_cache off;
                    proxy_read_timeout 1h;
                  '';
                };
              };
            };

            systemd.tmpfiles.settings.nixcon-quiz =
              lib.mkIf (cfg.questions == "/var/lib/nixcon-quiz/questions")
                {
                  "/var/lib/nixcon-quiz".d = {
                    mode = "0755";
                    user = "root";
                    group = "root";
                  };
                };
          };
        };
    in
    {
      packages = forAllSystems (pkgs: rec {
        nixcon-quiz = mkQuiz pkgs;
        default = nixcon-quiz;
        # Terminal UI for reviewing the question files; stdlib Python only.
        review = pkgs.writeShellScriptBin "nixcon-quiz-review" ''
          exec ${pkgs.python3}/bin/python3 ${./scripts/review.py} "$@"
        '';
      });

      apps = forAllSystems (pkgs: {
        review = {
          type = "app";
          program = pkgs.lib.getExe self.packages.${pkgs.stdenv.hostPlatform.system}.review;
        };
      });

      overlays.default = final: _prev: { nixcon-quiz = mkQuiz final; };

      formatter = forAllSystems (pkgs: pkgs.nixfmt-tree);

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
            pkgs.rust-analyzer
          ];
        };
      });

      checks = forAllSystems (
        pkgs:
        let
          quiz = self.packages.${pkgs.stdenv.hostPlatform.system}.nixcon-quiz;
        in
        {
          package = quiz;
          example-questions = pkgs.runCommand "example-questions-valid" { } ''
            ${pkgs.lib.getExe quiz} --check ${./examples/questions} && touch $out
          '';
        }
        // nixpkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          nixos = pkgs.testers.runNixOSTest {
            name = "nixcon-quiz";
            nodes.machine = {
              imports = [ quizModule ];
              services.nixcon-quiz = {
                enable = true;
                package = quiz;
                domain = "quiz.test";
                title = "Test Quiz";
                questionSeconds = 60;
                questions = pkgs.writeTextDir "only.toml" ''
                  question = "The only question"
                  choices = ["yes", "no"]
                  answer = "yes"
                  explanation = "It is the only question."
                '';
              };
              services.nginx.virtualHosts."quiz.test" = {
                enableACME = false;
                forceSSL = false;
              };
              networking.hosts."127.0.0.1" = [ "quiz.test" ];
              environment.systemPackages = [ pkgs.curl ];
            };
            testScript = ''
              import re

              machine.wait_for_unit("nixcon-quiz.service")
              machine.wait_for_unit("nginx.service")
              machine.wait_for_open_port(80)

              def find(pattern, text):
                  match = re.search(pattern, text)
                  assert match, f"{pattern} not in {text}"
                  return match.group(1)

              # The page arrives rendered, with the question and a player cookie.
              page = machine.succeed("curl -sf -c /tmp/jar http://quiz.test/")
              assert "The only question" in page, page
              assert "<title>Test Quiz</title>" in page, page
              htmx = find(r'src="(/vendor/htmx-[^"]+)"', page)
              machine.succeed(f"curl -sf http://quiz.test{htmx} | grep -q htmx")

              # The page's own stream URL names the rendered state, so the
              # stream stays quiet instead of re-rendering the same phase.
              stream = find(r'sse-connect="([^"]+)"', page)
              quiet = machine.succeed(
                f"curl -sN -b /tmp/jar --max-time 2 'http://quiz.test{stream}' || true"
              )
              assert "data:" not in quiet, quiet

              # The first event arrives through nginx right away, not when a
              # proxy buffer happens to fill; it is the same player's fragment.
              out = machine.succeed(
                "curl -sN -b /tmp/jar --max-time 3 http://quiz.test/api/events || true"
              )
              fragment = next(l for l in out.splitlines() if l.startswith("data:"))
              assert 'data-phase="question"' in fragment, fragment
              started = int(find(r'data-started="(\d+)"', fragment))
              ends = int(find(r'data-ends="(\d+)"', fragment))
              assert ends - started == 60000, fragment
              assert "It is the only question." not in page + fragment, "answer leaked"
              seq = int(find(r'name="seq" value="(\d+)"', fragment))

              def answer(seq, choice):
                  return machine.succeed(
                      "curl -s -o /dev/null -w '%{http_code}' -b /tmp/jar "
                      f"-d seq={seq} -d choice={choice} http://quiz.test/api/answer"
                  )

              assert answer(seq, 0) == "204"
              assert answer(seq, 1) == "204", "changing the answer"
              assert answer(seq + 1, 0) == "409", "not the open question"

              # A reload shows the saved answer.
              page = machine.succeed("curl -sf -b /tmp/jar http://quiz.test/")
              assert 'value="1" checked' in page, page

              # The livestream screen joins nobody, points at the public URL,
              # and its stream counts answers without revealing them.
              headers = machine.succeed("curl -sf -D - -o /tmp/spectate http://quiz.test/spectate")
              assert "set-cookie" not in headers.lower(), headers
              screen = machine.succeed("cat /tmp/spectate")
              assert "quiz.test</p>" in screen, screen
              assert "1 answered" in screen, screen
              assert "It is the only question." not in screen, "answer leaked"
              out = machine.succeed(
                "curl -sN --max-time 2 http://quiz.test/api/events/spectate || true"
              )
              assert 'data-phase="question"' in out, out
            '';
          };
        }
      );

      nixosModules = {
        default = quizModule;
        nixcon-quiz = quizModule;
      };
    };
}
