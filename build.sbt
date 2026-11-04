version := "0.1.1-SNAPSHOT"
scalaVersion := "3.8.4"
organization := "com.eed3si9n"

LocalRootProject / name := "scruff-root"
LocalRootProject / Compile / sources := Nil

lazy val plugin = projectMatrix
  .enablePlugins(SbtPlugin)
  .settings(
    name := "sbt-scruff",
    scalacOptions += "-Xsource:3",
    scriptedLaunchOpts ++= Seq(
      s"-Dplugin.version=${version.value}",
      // Built with `cargo build --release`.
      s"-Dscruff.binary=${(LocalRootProject / baseDirectory).value / "target" / "release" / "scruff"}",
    ),
    scriptedBufferLog := false,
    pluginCrossBuild / sbtVersion := {
      scalaBinaryVersion.value match
        case "2.12" => "1.5.8"
        case _      => "2.0.0"
    },
    scriptedSbt := {
      scalaBinaryVersion.value match
        case "2.12" => "1.10.6"
        case _      => (pluginCrossBuild / sbtVersion).value
    },
    scalacOptions ++= {
      scalaBinaryVersion.value match
        case "2.12" =>
          Seq("-Xsource:3", "-release:8", "-Xlint")
        case "3" => Nil
    },

  )
  .jvmPlatform(
    scalaVersions = Seq("3.8.4", "2.12.20")
  )

scmInfo := Some(
  ScmInfo(url("https://github.com/eed3si9n/scruff"), "scm:git@github.com:eed3si9n/scruff.git")
)
developers := List(
  Developer(
    id = "eed3si9n",
    name = "Eugene Yokota",
    email = "@eed3si9n",
    url = url("https://eed3si9n.com/"),
  )
)
pomIncludeRepository := { _ =>
  false
}
publishTo := {
  val centralSnapshots = "https://central.sonatype.com/repository/maven-snapshots/"
  if version.value.endsWith("-SNAPSHOT") then Some("central-snapshots" at centralSnapshots)
  else localStaging.value
}
homepage := Some(url("https://github.com/eed3si9n/scruff"))
licenses := List(License.MIT)
