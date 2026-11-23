#!/usr/bin/env bash
# Run the SDK installation journey on a disposable macOS machine.
# Usage: scripts/test-macos-install.sh <release directory>
set -euo pipefail
[[ $# -eq 1 ]] || { echo "Usage: $0 <release directory>" >&2; exit 2; }
release=$(cd "$1" && pwd)
manifest() { python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$release/release.json" "$1"; }
fail() { echo "$1" >&2; exit 1; }
[[ $(manifest runtimeIdentifier) == osx-arm64 ]] || fail 'The journey requires a macOS arm64 release.'
version=$(manifest packageVersion)
root=$(cd "$(dirname "$0")/.." && pwd)
runs=$root/.test-runs/osx-arm64/installation
mkdir -p "$runs"
workspace=$(mktemp -d "$runs/XXXXXXXX")
# .NET puts Unix sockets under TMPDIR and macOS caps a socket path at 104 bytes,
# which a directory inside the workspace would exceed.
TMPDIR=$(mktemp -d /tmp/isoplane-install.XXXXXX)
export TMPDIR
export DOTNET_CLI_HOME=$workspace
export ISOPLANE_HOME=$workspace/installation
export NUGET_PACKAGES=$workspace/packages
cli=$workspace/tools/isoplane
customer_file=$ISOPLANE_HOME/customer-owned.txt

teardown() {
    local status=$?
    trap - EXIT
    cd /
    if { [[ ! -e $ISOPLANE_HOME/installation.json ]] || "$cli" uninstall-sdk; } && rm -f "$customer_file"; then
        rm -rf "$workspace" "$TMPDIR"
    else
        echo "SDK teardown failed; recovery manifest and tool retained at $workspace" >&2
        status=1
    fi
    exit "$status"
}
trap teardown EXIT

test_http_guest() {
    dotnet run --project "$workspace/Consumer" --no-restore --disable-build-servers -- "$PWD" "$cli" "$2"
    echo "PASS: $1 through the installed testing package"
}

cd "$workspace"
for file in Directory.Build.props Directory.Build.targets Directory.Packages.props; do
    echo '<Project />' > "$file"
done
cp "$root/global.json" global.json
feed=$(python3 -c 'import sys; from xml.sax.saxutils import quoteattr; print(quoteattr(sys.argv[1]))' "$release/packages")
cat > nuget.config <<EOF
<configuration>
  <packageSources><clear/><add key="release" value=$feed/><add key="nuget" value="https://api.nuget.org/v3/index.json"/></packageSources>
  <packageSourceMapping><packageSource key="release"><package pattern="Isoplane.*"/></packageSource><packageSource key="nuget"><package pattern="*"/></packageSource></packageSourceMapping>
</configuration>
EOF
dotnet tool install Isoplane.Cli --tool-path "$workspace/tools" --configfile "$workspace/nuget.config" --version "$version"
"$cli" setup --source "$release" --yes
"$cli" setup --source "$release" --yes
debug_protocol_library=$ISOPLANE_HOME/debugger/Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.dll
[[ -e $debug_protocol_library ]] || fail 'Setup did not install the debug protocol library after its license was accepted.'
dotnet new isoplane-web --name Sample
mkdir Consumer
cat > Consumer/Consumer.csproj <<EOF
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><OutputType>Exe</OutputType><TargetFramework>net10.0</TargetFramework><ImplicitUsings>enable</ImplicitUsings></PropertyGroup>
  <ItemGroup><PackageReference Include="Isoplane.Testing" Version="$version" /></ItemGroup>
</Project>
EOF
cat > Consumer/Program.cs <<'EOF'
using Isoplane.Testing;
using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(10));
var app = await IsoplaneApplication.StartAsync(args[0],
    new ApplicationOptions { CliPath = args[1], Configuration = args[2] }, deadline.Token);
var address = app.Client.BaseAddress!;
try
{
    using var response = await app.Client.GetAsync("/", deadline.Token);
    var body = await response.Content.ReadAsStringAsync(deadline.Token);
    if (!response.IsSuccessStatusCode || body != "Hello from Isoplane!")
        throw new Exception($"The sandbox returned an unexpected response: {response.StatusCode} {body}\n{app.DiagnosticOutput}");
}
finally { await app.DisposeAsync(); }
using var client = new HttpClient();
try { await client.GetAsync(address, deadline.Token); }
catch (HttpRequestException) { return; }
throw new Exception("Disposal left the application listener running.");
EOF
dotnet restore Consumer/Consumer.csproj
cd Sample
dotnet build --disable-build-servers
test_http_guest Mono Debug
dotnet build -c Release --disable-build-servers
test_http_guest Release Release
template_package=$ISOPLANE_HOME/packages/Isoplane.Templates.$version.nupkg
rm "$template_package"
"$cli" setup --source "$release" --yes
[[ -e $template_package ]] || fail 'Repair did not restore the missing package.'
echo 'Customer content' > "$customer_file"
"$cli" uninstall-sdk
[[ ! -e $template_package ]] || fail 'Removal left the owned package installed.'
[[ ! -e $debug_protocol_library ]] || fail 'Removal left the debug protocol library installed.'
[[ -e $customer_file ]] || fail 'Removal deleted customer-owned content.'
rm "$customer_file"
"$cli" setup --source "$release" --yes
"$cli" doctor
"$cli" uninstall-sdk
echo 'PASS: install, repeat setup, both guest profiles, repair, removal, preservation, reinstall'
