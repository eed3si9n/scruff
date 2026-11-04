package sbtscruff

import sbt.*
import sbt.Keys.*
import sbt.internal.util.MessageOnlyException
import scala.sys.process.{ Process, ProcessLogger }

/** Formats the whole build with the scruff command-line formatter. */
object ScruffPlugin extends AutoPlugin {
  override def trigger = allRequirements

  object autoImport {
    @transient
    val scruffFmt = taskKey[Unit]("Formats the Scala files in the build with scruff")
    @transient
    val scruffFmtCheck = taskKey[Unit](
      "Fails if scruffFmt would change a file, showing how; writes nothing"
    )
    val scruffFmtOnCompile = settingKey[Boolean](
      "Whether compiling formats the configuration's sources with scruff first"
    )
    val scruffVersion = settingKey[String](
      "The scruff release scruffBinary downloads; by default the version in .scruff.toml"
    )
    @transient
    val scruffBinary = taskKey[File](
      "The scruff executable to run; scruffVersion's release by default"
    )
  }

  /** Formats the configuration's sources, for `compile`, when one has
    * changed since the last time.
    */
  @transient
  private val scruffFmtSources = taskKey[Unit]("Formats the configuration's sources with scruff")

  import autoImport.*

  /** The scruff version when .scruff.toml doesn't name one. */
  val DefaultVersion = "0.1.0"

  private val VersionLine = """\s*version\s*=\s*["']([^"']*)["'].*""".r

  /** The top-level `version` in `config`, a .scruff.toml, if it has one.
    * Lines after the first `[table]` belong to that table, so they're not
    * looked at.
    */
  def configVersion(config: File): Option[String] =
    if (!config.isFile) None
    else
      IO
        .readLines(config)
        .iterator
        .takeWhile(!_.trim.startsWith("["))
        .collectFirst { case VersionLine(v) if v.nonEmpty => v }

  override def globalSettings: Seq[Def.Setting[?]] = Seq(
    scruffFmtOnCompile := false
  )

  override def buildSettings: Seq[Def.Setting[?]] = Seq(
    scruffVersion := configVersion((ThisBuild / baseDirectory).value / ".scruff.toml")
      .getOrElse(DefaultVersion),
    scruffBinary := ScruffDownload.binary(scruffVersion.value, streams.value.log),
    scruffFmt := format(scruffBinary.value, (ThisBuild / baseDirectory).value, streams.value.log),
    scruffFmtCheck := check(
      scruffBinary.value,
      (ThisBuild / baseDirectory).value,
      streams.value.log,
    ),
  )

  override def projectSettings: Seq[Def.Setting[?]] =
    inConfig(Compile)(onCompileSettings) ++ inConfig(Test)(onCompileSettings)

  private def onCompileSettings: Seq[Def.Setting[?]] =
    Seq(
      scruffFmtSources := Def.taskDyn {
        val on = scruffFmtOnCompile.value
        val log = streams.value.log
        val stampFile = streams.value.cacheDirectory / "source-stamps"
        // Directories rather than files, so scruff skips what its
        // configuration excludes and what `.gitignore` leaves out.
        val dirs = unmanagedSourceDirectories.value.filter(_.isDirectory)
        val sources = unmanagedSources.value
        val unchanged = stampFile.isFile && IO.read(stampFile) == stamps(sources)
        if (!on || dirs.isEmpty || unchanged) Def.task(())
        else Def.task {
          formatSources(scruffBinary.value, (ThisBuild / baseDirectory).value, dirs, log)
          // After formatting, so the files scruff rewrote count as seen.
          IO.write(stampFile, stamps(sources))
        }
      }.value,
      compile / compileInputs := (compile / compileInputs).dependsOn(scruffFmtSources).value,
    )

  /** Each source and when it was last modified, to tell whether one has
    * changed since scruff formatted them.
    */
  private def stamps(sources: Seq[File]): String = sources
    .map(f => s"${f.getAbsolutePath}\t${f.lastModified}")
    .sorted
    .mkString("\n")


  /** Runs `binary format` on `root`, failing if scruff can't run or exits
    * with an error.
    */
  def format(binary: File, root: File, log: Logger): Unit =
    run(binary, root, log, Nil, Seq(".")) match {
      case 0    => ()
      case exit => throw new MessageOnlyException(s"scruff exited with $exit")
    }

  /** Runs `binary format --check` on `root`, which writes nothing and shows
    * how each file would change, failing if any would.
    */
  def check(binary: File, root: File, log: Logger): Unit =
    run(binary, root, log, Seq("--check"), Seq(".")) match {
      case 0 => ()
      // scruff exits with 1 when a file would change, and with 2 when one
      // can't be formatted.
      case 1    => throw new MessageOnlyException("some files aren't formatted; run scruffFmt")
      case exit => throw new MessageOnlyException(s"scruff exited with $exit")
    }

  /** Runs `binary format` on `dirs` before compiling. A file scruff can't
    * format is only a warning: the compiler reports what's wrong with it.
    */
  def formatSources(binary: File, root: File, dirs: Seq[File], log: Logger): Unit =
    if (dirs.nonEmpty) {
      val paths = dirs.map(dir => IO.relativize(root, dir).getOrElse(dir.getAbsolutePath))
      run(binary, root, log, Nil, paths) match {
        case 0    => ()
        case exit => log.warn(s"scruff exited with $exit; compiling the sources as they are")
      }
    }

  /** Runs `binary format` in `root` on `paths`, relative to `root` so files
    * show relative to the build's root.
    */
  private def run(
      binary: File,
      root: File,
      log: Logger,
      options: Seq[String],
      paths: Seq[String],
  ): Int = {
    val command = Seq(binary.getAbsolutePath, "format", "--color", "always") ++ options ++ paths
    try Process(command, root).!(ProcessLogger(log.info(_), log.info(_)))
    catch {
      case e: java.io.IOException =>
        throw new MessageOnlyException(s"can't run $binary: ${e.getMessage}")
    }
  }
}
