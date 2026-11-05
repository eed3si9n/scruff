sys.props.get("plugin.version") match {
  case Some(v) => addSbtPlugin("com.eed3si9n" % "sbt-scruff" % v)
  case _       => sys.error("The system property 'plugin.version' is not defined.")
}
