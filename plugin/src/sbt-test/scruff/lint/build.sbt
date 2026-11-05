ThisBuild / scalaVersion := "3.8.4"

// scruff, through a script that logs each run to runsLog.
lazy val runsLog = settingKey[File]("")
ThisBuild / runsLog := (ThisBuild / baseDirectory).value / "target" / "scruff-runs"
ThisBuild / scruffBinary := {
  val wrapper = (ThisBuild / baseDirectory).value / "target" / "scruff-logging"
  IO.write(
    wrapper,
    s"""#!/bin/sh
       |echo "$$*" >> "${runsLog.value}"
       |exec "${sys.props("scruff.binary")}" "$$@"
       |""".stripMargin,
  )
  wrapper.setExecutable(true)
  wrapper
}

lazy val root = (project in file(".")).aggregate(sub)
lazy val sub = project

// `checkRuns n`: scruff ran n times since `resetRuns`.
@transient
lazy val resetRuns = taskKey[Unit]("")
lazy val checkRuns = inputKey[Unit]("")

resetRuns := IO.delete(runsLog.value)

checkRuns := {
  val n = Def.spaceDelimited().parsed.head.toInt
  val runs = if (runsLog.value.isFile) IO.readLines(runsLog.value) else Nil
  if (runs.size != n) sys.error(s"scruff ran ${runs.size} times, not $n: $runs")
}
