package sbtscruff

import sbt.*
import sbt.Keys.*
import sbt.internal.util.{ MessageOnlyException, Terminal }
import scala.sys.process.{ Process, ProcessLogger }

/** Formats and lints the whole build with the scruff command-line tool. */
object ScruffPlugin extends AutoPlugin {
  override def trigger = allRequirements

  object autoImport {
    @transient
    val scruffFmt = taskKey[Unit]("Formats the Scala files in the build with scruff")
    @transient
    val scruffFmtCheck = taskKey[Unit](
      "Fails if scruffFmt would change a file, showing how; writes nothing"
    )
    val scruff = inputKey[Unit](
      "Runs scruff in the build's root with the given arguments, such as `scruff check`"
    )
    val scruffFmtOnCompile = settingKey[Boolean](
      "Whether compiling formats the configuration's sources with scruff first"
    )
    val scruffCheckOnCompile = settingKey[Boolean](
      "Whether compiling checks the configuration's sources against scruff's lint rules first"
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
   *  changed since the last time.
   */
  @transient
  private val scruffFmtSources = taskKey[Unit]("Formats the configuration's sources with scruff")

  /** Checks the configuration's sources against the lint rules, for
   *  `compile`, after formatting them, when one has changed since they last
   *  passed.
   */
  @transient
  private val scruffCheckSources = taskKey[Unit](
    "Checks the configuration's sources against scruff's lint rules"
  )

  import autoImport.*

  private val VersionLine = """\s*version\s*=\s*["']([^"']*)["'].*""".r

  /** The top-level `version` in `config`, a .scruff.toml, if it has one.
   *  Lines after the first `[table]` belong to that table, so they're not
   *  looked at.
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
    scruffFmtOnCompile := false,
    scruffCheckOnCompile := false,
  )

  override def buildSettings: Seq[Def.Setting[?]] =
    Seq(
      scruffVersion := configVersion((ThisBuild / baseDirectory).value / ".scruff.toml")
        .getOrElse(ScruffBuildInfo.scruffVersion),
      scruffBinary := ScruffDownload.binary(scruffVersion.value, streams.value.log),
      scruffFmt := format(scruffBinary.value, (ThisBuild / baseDirectory).value, streams.value.log),
      scruffFmtCheck := check(
        scruffBinary.value,
        (ThisBuild / baseDirectory).value,
        streams.value.log,
      ),
      scruff := {
        val args = Def.spaceDelimited("<args>").parsed
        runArgs(scruffBinary.value, (ThisBuild / baseDirectory).value, streams.value.log, args)
      },
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
      scruffCheckSources := Def.taskDyn {
        // First, so the files are checked as formatting leaves them.
        scruffFmtSources.value
        val on = scruffCheckOnCompile.value
        val log = streams.value.log
        val stampFile = streams.value.cacheDirectory / "lint-stamps"
        val dirs = unmanagedSourceDirectories.value.filter(_.isDirectory)
        val sources = unmanagedSources.value
        val unchanged = stampFile.isFile && IO.read(stampFile) == stamps(sources)
        if (!on || dirs.isEmpty || unchanged) Def.task(())
        else Def.task {
          // Only when they pass, so problems are shown again next time.
          if (lintSources(scruffBinary.value, (ThisBuild / baseDirectory).value, dirs, log))
            IO.write(stampFile, stamps(sources))
        }
      }.value,
      compile / compileInputs := (compile / compileInputs).dependsOn(scruffCheckSources).value,
    )

  /** Each source and when it was last modified, to tell whether one has
   *  changed since scruff formatted them.
   */
  private def stamps(sources: Seq[File]): String = sources
    .map(f => s"${f.getAbsolutePath}\t${f.lastModified}")
    .sorted
    .mkString("\n")


  /** Runs `binary format` on `root`, failing if scruff can't run or exits
   *  with an error.
   */
  def format(binary: File, root: File, log: Logger): Unit =
    run(binary, root, log, "format", Nil, Seq(".")) match {
      case 0    => ()
      case exit => throw new MessageOnlyException(s"scruff exited with $exit")
    }

  /** Runs `binary format --check` on `root`, which writes nothing and shows
   *  how each file would change, failing if any would.
   */
  def check(binary: File, root: File, log: Logger): Unit =
    run(binary, root, log, "format", Seq("--check"), Seq(".")) match {
      case 0 => ()
      // scruff exits with 1 when a file would change, and with 2 when one
      // can't be formatted.
      case 1    => throw new MessageOnlyException("some files aren't formatted; run scruffFmt")
      case exit => throw new MessageOnlyException(s"scruff exited with $exit")
    }

  /** Runs `binary` with `args` in `root`, as on the command line, failing
   *  if it exits with an error.
   */
  def runArgs(binary: File, root: File, log: Logger, args: Seq[String]): Unit =
    // Through the environment, so it works whatever the command is; a
    // `--color` in `args` still wins.
    exec(
      binary,
      args,
      root,
      log,
      (if (colorEnabled) "CLICOLOR_FORCE" else "NO_COLOR") -> "1",
    ) match {
      case 0    => ()
      case exit => throw new MessageOnlyException(s"scruff exited with $exit")
    }

  /** Runs `binary format` on `dirs` before compiling. A file scruff can't
   *  format is only a warning: the compiler reports what's wrong with it.
   */
  def formatSources(binary: File, root: File, dirs: Seq[File], log: Logger): Unit =
    if (dirs.nonEmpty) {
      val paths = dirs.map(dir => IO.relativize(root, dir).getOrElse(dir.getAbsolutePath))
      run(binary, root, log, "format", Nil, paths) match {
        case 0    => ()
        case exit => log.warn(s"scruff exited with $exit; compiling the sources as they are")
      }
    }

  /** Runs `binary check` on `dirs` before compiling, failing if a lint rule
   *  flags anything. A file scruff can't check is only a warning: the
   *  compiler reports what's wrong with it. Returns whether all passed.
   */
  def lintSources(binary: File, root: File, dirs: Seq[File], log: Logger): Boolean =
    dirs.isEmpty || {
      val paths = dirs.map(dir => IO.relativize(root, dir).getOrElse(dir.getAbsolutePath))
      run(binary, root, log, "check", Nil, paths) match {
        case 0    => true
        case 1    => throw new MessageOnlyException("scruff's lint rules flagged problems")
        case exit =>
          log.warn(s"scruff exited with $exit; compiling the sources as they are")
          false
      }
    }

  /** Runs `binary command` in `root` on `paths`, relative to `root` so files
   *  show relative to the build's root.
   */
  private def run(
      binary: File,
      root: File,
      log: Logger,
      command: String,
      options: Seq[String],
      paths: Seq[String],
  ): Int = {
    val color = if (colorEnabled) "always" else "never"
    exec(binary, Seq(command, "--color", color) ++ options ++ paths, root, log)
  }

  /** Whether sbt colors its output, as `sbt.color`, `NO_COLOR` and the
   *  terminal sbt started in decide. scruff's output goes to sbt's log
   *  rather than the terminal, so scruff can't tell for itself.
   */
  private def colorEnabled: Boolean = Terminal.console.isColorEnabled

  /** Runs `binary` with `args` in `root`, logging what it prints, and
   *  returns its exit code.
   */
  private def exec(
      binary: File,
      args: Seq[String],
      root: File,
      log: Logger,
      env: (String, String)*,
  ): Int =
    try Process(binary.getAbsolutePath +: args, root, env*).!(
      ProcessLogger(log.info(_), log.info(_))
    )
    catch {
      case e: java.io.IOException =>
        throw new MessageOnlyException(s"can't run $binary: ${e.getMessage}")
    }
}
