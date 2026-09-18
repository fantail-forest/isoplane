using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Security.Cryptography;
using System.Threading;
using Microsoft.Build.Framework;
using Microsoft.Build.Utilities;
using Task = Microsoft.Build.Utilities.Task;

/// <summary>Publishes with the local Linux compiler and returns its WASM output.</summary>
public sealed class IsoplaneContainerPublish : Task, ICancelableTask
{
    /// <summary>The guest project to publish.</summary>
    [Required] public string ProjectPath { get; set; }
    /// <summary>The source tree containing the project and its references.</summary>
    [Required] public string SourceRoot { get; set; }
    /// <summary>The installed Isoplane SDK directory.</summary>
    [Required] public string SdkDirectory { get; set; }
    /// <summary>The local feed supplying the source-built runtime packs.</summary>
    [Required] public string GuestRuntimeFeed { get; set; }
    /// <summary>The unpacked WASI SDK named by the selected runtime feed.</summary>
    [Required] public string WasiSdkPackDirectory { get; set; }
    /// <summary>The host-native CLI that validates the linked artifact before publication.</summary>
    [Required] public string Validator { get; set; }

    /// <summary>Explicit admission memory ceiling in bytes, independent of other plan limits.</summary>
    public string GuestMemoryBytes { get; set; }
    /// <summary>The declared linker memory maximum in bytes.</summary>
    public string MemoryMaxBytes { get; set; }
    /// <summary>The local publish directory.</summary>
    [Required] public string PublishDirectory { get; set; }
    /// <summary>The selected build configuration.</summary>
    [Required] public string Configuration { get; set; }
    /// <summary>Disables network access and requires provisioned build inputs.</summary>
    public bool Offline { get; set; }
    /// <summary>Optional root for the per-image compiler package cache.</summary>
    public string CacheDirectory { get; set; }
    /// <summary>Maximum wait for a container runtime control operation.</summary>
    [Required] public int CommandTimeoutSeconds { get; set; }
    /// <summary>The published module, for native admission validation.</summary>
    [Output] public string Module { get; set; }

    private readonly CancellationTokenSource cancellation = new CancellationTokenSource();
    private readonly string owner = Guid.NewGuid().ToString("N");
    private bool runtimeAvailable;

    /// <summary>Stops compilation and lets Execute reclaim its container.</summary>
    public void Cancel() { cancellation.Cancel(); }

    /// <summary>Builds and copies the module, failing on missing tools or compiler errors.</summary>
    public override bool Execute()
    {
        string scratch = Path.Combine(Path.GetTempPath(), "isoplane-publish-" + owner);
        bool success = false;
        try
        {
            Directory.CreateDirectory(scratch);
            Publish(scratch);
            success = true;
        }
        catch (OperationCanceledException)
        {
            Log.LogError("isoplane: C# publication cancelled.");
        }
        catch (Exception error) when (error is IOException || error is Win32Exception || error is InvalidOperationException)
        {
            Log.LogError("isoplane: " + error.Message);
        }
        finally
        {
            if (runtimeAvailable && !CleanupContainer())
            {
                success = false;
                Log.LogError("isoplane: build files retained at {0} because compiler cleanup failed.", scratch);
            }
            else
            {
                try { Directory.Delete(scratch, true); }
                catch (IOException error) { Log.LogWarning("isoplane: cleaning build directory {0}: {1}", scratch, error.Message); }
            }
        }
        return success && !Log.HasLoggedErrors;
    }

    private void Publish(string scratch)
    {
        if (CommandTimeoutSeconds <= 0)
            throw new InvalidOperationException("IsoplaneContainerCommandTimeoutSeconds must be positive.");
        SourceRoot = Path.GetFullPath(SourceRoot).TrimEnd(Path.DirectorySeparatorChar);
        ProjectPath = Path.GetFullPath(ProjectPath);
        SdkDirectory = Path.GetFullPath(SdkDirectory).TrimEnd(Path.DirectorySeparatorChar);
        if (!ProjectPath.StartsWith(SourceRoot + Path.DirectorySeparatorChar, StringComparison.Ordinal))
            throw new InvalidOperationException("IsoplaneBuildRoot must contain the guest project and its project references.");

        GuestRuntimeFeed = Path.GetFullPath(GuestRuntimeFeed).TrimEnd(Path.DirectorySeparatorChar);
        if (!File.Exists(Path.Combine(GuestRuntimeFeed, "Isoplane.GuestRuntime.props")))
            throw new InvalidOperationException("The guest runtime feed has no Isoplane.GuestRuntime.props: " + GuestRuntimeFeed);

        WasiSdkPackDirectory = Path.GetFullPath(WasiSdkPackDirectory).TrimEnd(Path.DirectorySeparatorChar);
        if (!File.Exists(Path.Combine(WasiSdkPackDirectory, "Sdk", "Sdk.targets")))
            throw new InvalidOperationException("The runtime feed's unpacked WASI SDK is missing: " + WasiSdkPackDirectory);

        if (Run("docker", new[] { "info" }, false, true).ExitCode != 0)
            throw new InvalidOperationException("Docker is unavailable. Install and start Docker Desktop (or a compatible local Docker runtime), then retry dotnet publish.");
        runtimeAvailable = true;

        string image = CompilerImage();
        if (Run("docker", new[] { "image", "inspect", image }, false, true).ExitCode != 0)
        {
            if (Offline)
                throw new InvalidOperationException("offline compiler image is missing. Run dotnet publish once with network access to provision " + image + ".");
            Require(Run("docker", new[] { "build", "--tag", image, Path.Combine(SdkDirectory, "Build") }, true), "building the pinned Linux compiler image");
        }

        var user = Run("id", new[] { "-u" }, false, true);
        var group = Run("id", new[] { "-g" }, false, true);
        Require(user, "establishing the developer's user ID");
        Require(group, "establishing the developer's group ID");
        string cacheRoot = string.IsNullOrEmpty(CacheDirectory)
            ? Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Isoplane")
            : Path.GetFullPath(CacheDirectory);
        string cache = Path.Combine(cacheRoot, image.Replace(':', '-'), "nuget");
        Directory.CreateDirectory(cache);
        var args = new List<string> {
            "create", "--pull", "never", "--name", "isoplane-publish-" + owner, "--label", "isoplane.publish=" + owner,
            "--user", user.Output.Trim() + ":" + group.Output.Trim(), "--init", "--workdir", Path.GetDirectoryName(ProjectPath),
            "--volume", SourceRoot + ":" + SourceRoot + ":ro",
            "--volume", scratch + ":/build", "--volume", cache + ":/nuget",
            // NuGet coordinates concurrent package extraction through its scratch directory.
            "--env", "NUGET_SCRATCH=/nuget/.scratch",
            "--env", "NUGET_HTTP_CACHE_PATH=/nuget/.http-cache",
        };
        if (!SdkDirectory.StartsWith(SourceRoot + "/", StringComparison.Ordinal) && SdkDirectory != SourceRoot)
            args.AddRange(new[] { "--volume", SdkDirectory + ":" + SdkDirectory + ":ro" });
        args.AddRange(new[] { "--volume", GuestRuntimeFeed + ":" + GuestRuntimeFeed + ":ro" });
        args.AddRange(new[] { "--volume", WasiSdkPackDirectory + ":" + WasiSdkPackDirectory + ":ro" });
        if (Offline) args.AddRange(new[] { "--network", "none" });
        args.AddRange(new[] { image, "dotnet", "publish", ProjectPath, "--configuration", Configuration, "--disable-build-servers" });
        foreach (var property in ((IBuildEngine6)BuildEngine).GetGlobalProperties())
        {
            if (property.Key.StartsWith("_", StringComparison.Ordinal)) continue;
            args.Add("-p:" + property.Key + "=" + property.Value);
        }
        args.AddRange(new[] {
            "-p:IsoplaneContainerBuild=false", "-p:_IsoplaneContainerIntermediate=true",
            "-p:IsoplaneGuestRuntimeFeed=" + GuestRuntimeFeed + "/",
            "-p:UseArtifactsOutput=true", "-p:ArtifactsPath=/build/artifacts",
            "-p:PublishDir=/build/publish/", "-p:NuGetAudit=false",
            "-p:UseSharedCompilation=false", "-nodeReuse:false",
        });
        if (!string.IsNullOrEmpty(MemoryMaxBytes)) args.Add("-p:IsoplaneMemoryMaxBytes=" + MemoryMaxBytes);
        if (!string.IsNullOrEmpty(GuestMemoryBytes)) args.Add("-p:IsoplaneGuestMemoryBytes=" + GuestMemoryBytes);

        if (Offline)
        {
            Directory.CreateDirectory(Path.Combine(scratch, "empty-feed"));
            args.Add("-p:RestoreSources=/build/empty-feed");
            args.Add("-p:RestoreAdditionalProjectSources=");
        }
        Require(Run("docker", args, false, true), "creating the Linux compiler container");
        Log.LogMessage(MessageImportance.High, "isoplane: compiling in local container isoplane-publish-{0}", owner);
        Require(Run("docker", new[] { "start", "--attach", "isoplane-publish-" + owner }, true),
            Offline ? "offline compilation; a missing NuGet package must be provisioned by an online publish" : "compiling the C# guest");

        string[] modules = Directory.GetFiles(Path.Combine(scratch, "publish"), "*.wasm");
        if (modules.Length != 1)
            throw new InvalidOperationException("the compiler did not publish exactly one WASM module.");
        var validation = new List<string> { "doctor", modules[0], "--no-color" };

        if (!string.IsNullOrEmpty(GuestMemoryBytes)) validation.AddRange(new[] { "--guest-memory-bytes", GuestMemoryBytes });
        Require(Run(Validator, validation, true), "validating the linked module before publication");
        string identity = modules[0] + ".runtime";
        if (!File.Exists(identity))
            throw new InvalidOperationException("The compiler did not record the linked runtime pack identity.");
        PublishDirectory = Path.GetFullPath(Path.Combine(Path.GetDirectoryName(ProjectPath), PublishDirectory));
        Directory.CreateDirectory(PublishDirectory);
        Module = Path.GetFullPath(Path.Combine(PublishDirectory, Path.GetFileName(modules[0])));
        string pending = Path.Combine(PublishDirectory, "." + owner + ".wasm");
        try
        {
            File.Copy(modules[0], pending);
            try { File.Move(pending, Module); }
            catch (IOException) when (File.Exists(Module)) { File.Replace(pending, Module, null); }
        }
        finally
        {
            if (File.Exists(pending)) File.Delete(pending);
        }
        File.Copy(identity, Module + ".runtime", true);
        Log.LogMessage(MessageImportance.High, "isoplane: published {0}", Module);
    }

    private string CompilerImage()
    {
        using (var hash = SHA256.Create())
        {
            byte[] digest = hash.ComputeHash(File.ReadAllBytes(Path.Combine(SdkDirectory, "Build", "Dockerfile")));
            return "isoplane-csharp:" + BitConverter.ToString(digest).Replace("-", "").ToLowerInvariant().Substring(0, 20);
        }
    }

    private bool CleanupContainer()
    {
        try
        {
            var containers = Run("docker", new[] { "ps", "--all", "--quiet", "--filter", "label=isoplane.publish=" + owner }, false, true, true);
            if (containers.ExitCode != 0)
            {
                Log.LogError("isoplane: could not verify compiler cleanup: {0}", containers.Output);
                return false;
            }
            foreach (string id in containers.Output.Split(new[] { '\r', '\n' }, StringSplitOptions.RemoveEmptyEntries))
            {
                Require(Run("docker", new[] { "rm", "--force", id }, false, true, true), "removing the compiler container " + id);
                Log.LogMessage(MessageImportance.High, "isoplane: released compiler container {0}", id);
            }
            return true;
        }
        catch (Exception error) when (error is IOException || error is Win32Exception || error is InvalidOperationException)
        {
            Log.LogError("isoplane: compiler cleanup failed: {0}", error.Message);
            return false;
        }
    }

    private sealed class Result
    {
        internal int ExitCode;
        internal string Output;
    }

    private static void Require(Result result, string operation)
    {
        if (result.ExitCode != 0)
            throw new InvalidOperationException(operation + " failed (exit " + result.ExitCode + ").\n" + result.Output);
    }

    private Result Run(string executable, IEnumerable<string> arguments, bool stream, bool bounded = false, bool cleanup = false)
    {
        if (!cleanup) cancellation.Token.ThrowIfCancellationRequested();
        var start = new ProcessStartInfo(executable) { WorkingDirectory = Path.GetDirectoryName(ProjectPath), RedirectStandardOutput = true, RedirectStandardError = true, UseShellExecute = false };
        var command = new CommandLineBuilder();
        foreach (string argument in arguments) command.AppendSwitchIfNotNull("", argument);
        start.Arguments = command.ToString();
        using (var process = new Process { StartInfo = start })
        {
            var output = new System.Text.StringBuilder();
            DataReceivedEventHandler receive = (sender, line) => {
                if (line.Data == null) return;
                if (stream) Log.LogMessage(MessageImportance.High, line.Data);
                else lock (output) output.AppendLine(line.Data);
            };
            process.OutputDataReceived += receive;
            process.ErrorDataReceived += receive;
            try { process.Start(); }
            catch (Win32Exception error) when (executable == "docker")
            {
                throw new InvalidOperationException("Docker CLI is missing. Install Docker Desktop (or a compatible local Docker runtime) and put docker on PATH.", error);
            }
            process.BeginOutputReadLine();
            process.BeginErrorReadLine();
            var clock = Stopwatch.StartNew();
            while (!process.WaitForExit(100))
            {
                if ((!cleanup && cancellation.IsCancellationRequested) || (bounded && clock.Elapsed.TotalSeconds > CommandTimeoutSeconds))
                {
                    process.Kill();
                    process.WaitForExit();
                    if (!cleanup) cancellation.Token.ThrowIfCancellationRequested();
                    throw new InvalidOperationException(executable + " did not respond within " + CommandTimeoutSeconds + " seconds. Check the local container runtime.");
                }
            }
            process.WaitForExit();
            if (!cleanup) cancellation.Token.ThrowIfCancellationRequested();
            return new Result { ExitCode = process.ExitCode, Output = output.ToString() };
        }
    }
}
