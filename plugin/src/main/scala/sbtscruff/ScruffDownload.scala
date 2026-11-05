package sbtscruff

import gigahorse.Request
import java.io.File
import java.nio.file.{ Files, StandardCopyOption }
import java.security.MessageDigest
import sbt.internal.util.MessageOnlyException
import sbt.io.IO
import sbt.librarymanagement.Http
import sbt.util.Logger
import scala.concurrent.Await
import scala.concurrent.duration.*
import scala.sys.process.Process
import scala.util.control.NonFatal

/** Downloads scruff's release binaries from GitHub, checks them against the
 *  release's `SHA256SUMS`, and keeps them in a cache shared by every build.
 */
object ScruffDownload {
  val releases = "https://github.com/eed3si9n/scruff/releases/download"

  /** The scruff executable for `version`, downloaded unless it's cached. */
  def binary(version: String, log: Logger): File = {
    val platform = currentPlatform
    val exe = cacheDir / version / platform / executableName
    if (!exe.isFile) {
      log.info(s"downloading scruff $version for $platform")
      download(version, platform, exe)
    }
    exe
  }

  /** The platform in the release's archive names, which follow sbtn's. */
  def currentPlatform: String = {
    val os = sys.props("os.name").toLowerCase
    val arch = sys.props("os.arch").toLowerCase match {
      case "amd64" | "x86_64"  => "x86_64"
      case "aarch64" | "arm64" => "aarch64"
      case other               => other
    }
    if (os.startsWith("mac")) "universal-apple-darwin"
    // Windows on ARM runs x86_64 binaries.
    else if (os.startsWith("windows")) "x86_64-pc-win32"
    else if (os.startsWith("linux") && (arch == "x86_64" || arch == "aarch64")) s"$arch-pc-linux"
    else
      throw new MessageOnlyException(
        s"scruff has no release for ${sys.props("os.name")} on ${sys.props("os.arch")}; set scruffBinary to a scruff you've built"
      )
  }

  private def isWindows = sys.props("os.name").toLowerCase.startsWith("windows")
  private def executableName = if (isWindows) "scruff.exe" else "scruff"

  /** `$XDG_CACHE_HOME/scruff`, or `~/.cache/scruff`. */
  private def cacheDir: File = sys.env.get("XDG_CACHE_HOME").filter(_.nonEmpty) match {
    case Some(dir) => new File(dir) / "scruff"
    case None      => new File(sys.props("user.home")) / ".cache" / "scruff"
  }

  private def download(version: String, platform: String, exe: File): Unit = {
    val base = s"$releases/v$version"
    val archiveName =
      s"scruff-$platform-$version" + (if (platform.endsWith("win32")) ".zip" else ".tar.gz")
    IO.withTemporaryDirectory { tmp =>
      val sums = fetch(s"$base/SHA256SUMS", tmp / "SHA256SUMS", version)
      val expected = IO
        .readLines(sums)
        .map(_.trim.split("\\s+"))
        .collectFirst { case Array(sum, name) if name.stripPrefix("*") == archiveName => sum }
        .getOrElse(throw new MessageOnlyException(s"scruff $version has no $archiveName"))
      val archive = fetch(s"$base/$archiveName", tmp / archiveName, version)
      val actual = sha256(archive)
      if (!actual.equalsIgnoreCase(expected))
        throw new MessageOnlyException(
          s"$archiveName has SHA-256 $actual, but SHA256SUMS says $expected"
        )
      val out = tmp / "out"
      IO.createDirectory(out)
      if (archiveName.endsWith(".zip")) IO.unzip(archive, out)
      else {
        val exit = Process(Seq("tar", "-xzf", archive.getAbsolutePath, "-C", out.getAbsolutePath)).!
        if (exit != 0) throw new MessageOnlyException(s"can't extract $archiveName")
      }
      val extracted = out / archiveName.stripSuffix(".zip").stripSuffix(".tar.gz") / executableName
      if (!extracted.isFile) throw new MessageOnlyException(s"$archiveName has no $executableName")
      extracted.setExecutable(true)
      // Moved into place whole, so a build running at the same time never
      // sees half a file.
      IO.createDirectory(exe.getParentFile)
      val staged = exe.getParentFile / s".${exe.getName}.${System.nanoTime}"
      IO.copyFile(extracted, staged)
      staged.setExecutable(true)
      Files.move(staged.toPath, exe.toPath, StandardCopyOption.ATOMIC_MOVE)
    }
  }

  /** Downloads `url` to `to` with the HTTP client sbt itself uses, which
   *  follows GitHub's redirects to its download servers. The status is
   *  checked here: sbt 2's client doesn't fail on one, and sbt 1's
   *  `download` doesn't either.
   */
  private def fetch(url: String, to: File, version: String): File = {
    val response =
      try Await.result(Http.http.processFull(Request(url)), 10.minutes)
      catch {
        case NonFatal(e) => throw new MessageOnlyException(s"can't download $url: $e")
      }
    response.status match {
      case 200 =>
        val body = response.bodyAsByteBuffer
        val bytes = new Array[Byte](body.remaining)
        body.get(bytes)
        IO.write(to, bytes)
        to
      case 404 =>
        throw new MessageOnlyException(s"scruff $version isn't released: $url not found")
      case status =>
        throw new MessageOnlyException(s"can't download $url: $status ${response.statusText}")
    }
  }

  private def sha256(file: File): String = {
    val digest = MessageDigest.getInstance("SHA-256")
    digest.update(IO.readBytes(file))
    digest.digest().map(b => f"${b & 0xff}%02x").mkString
  }

  private implicit class FileOps(private val file: File) extends AnyVal {
    def /(name: String): File = new File(file, name)
  }
}
