scalaVersion := "3.8.4"
organization := "com.eed3si9n"

LocalRootProject / name := "scruff-root"
LocalRootProject / Compile / sources := Nil

lazy val plugin = projectMatrix
  .enablePlugins(SbtPlugin, BuildInfoPlugin)
  .settings(
    name := "sbt-scruff",
    buildInfoKeys := {
      val v = scruffDefaultVersion.value
      // An action, so a build without tags fails when the source is
      // generated, not at load.
      Seq[BuildInfoKey](BuildInfoKey.action("scruffVersion")(v.get))
    },
    buildInfoPackage := "sbtscruff",
    buildInfoObject := "ScruffBuildInfo",
    scalacOptions += "-Xsource:3",
    scriptedLaunchOpts ++= Seq(
      s"-Dplugin.version=${version.value}",
      s"-Dscruff.default.version=${scruffDefaultVersion.value.get}",
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
  ScmInfo(uri("https://github.com/eed3si9n/scruff"), "scm:git@github.com:eed3si9n/scruff.git")
)
developers := List(
  Developer(
    id = "eed3si9n",
    name = "Eugene Yokota",
    email = "@eed3si9n",
    url = uri("https://eed3si9n.com/"),
  )
)
pomIncludeRepository := { _ =>
  false
}
publishTo := {
  val centralSnapshots = "https://central.sonatype.com/repository/maven-snapshots/"
  if isSnapshot.value then Some("central-snapshots" at centralSnapshots)
  else localStaging.value
}
homepage := Some(uri("https://github.com/eed3si9n/scruff"))
licenses := List(License.MIT)

ThisBuild / dynverSonatypeSnapshots := true
// A snapshot is the tag it's based on, plus -SNAPSHOT: 0.2.0-SNAPSHOT, not
// 0.2.0+2-46df23a2+20261105-0741-SNAPSHOT.
ThisBuild / version := {
  val dynverVersion = (ThisBuild / version).value
  dynverGitDescribeOutput.value
    .filter(out => out.isSnapshot() && out.ref.isTag)
    .fold(dynverVersion)(out => s"${out.ref.dropPrefix}-SNAPSHOT")
}

/** The scruff release the plugin downloads by default: this version, or
 *  for a snapshot, the tag it's based on, since a snapshot isn't released.
 */
lazy val scruffDefaultVersion =
  settingKey[scala.util.Try[String]]("The scruff version the plugin defaults to")

ThisBuild / scruffDefaultVersion := {
  val v = version.value
  val snapshot = isSnapshot.value
  val out = dynverGitDescribeOutput.value
  scala.util.Try:
    if !snapshot then v
    else
      out
        .map(_.ref)
        .filter(_.isTag)
        .map(_.dropPrefix)
        .getOrElse(
          sys.error(s"$v is a snapshot, and no release tag (v*) was found to fall back to")
        )
}
