import SteinsDomain.Vectors

/-- `lake exe vectors` — print the differential vector file on stdout. -/
def main : IO Unit := IO.print SteinsDomain.Vectors.render
