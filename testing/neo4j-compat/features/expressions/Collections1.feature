Feature: Collection and projection patterns common in Neo4j apps

  Scenario: size on collected names returns list cardinality
    Given an empty graph
    And having executed:
      """
      MATCH (n) DETACH DELETE n;
      CREATE (:Person {name: 'Alice'});
      CREATE (:Person {name: 'Bob'});
      CREATE (:Person {name: 'Carol'});
      """
    When executing query:
      """
      MATCH (p:Person)
      WITH collect(p.name) AS names
      RETURN size(names) AS count_names
      """
    Then the result should be, in any order:
      | count_names |
      | 3           |

  Scenario: list comprehension filters collected values
    Given an empty graph
    And having executed:
      """
      MATCH (n) DETACH DELETE n;
      CREATE (:Person {name: 'Alice'});
      CREATE (:Person {name: 'Bob'});
      CREATE (:Person {name: 'Anna'});
      """
    When executing query:
      """
      MATCH (p:Person)
      WITH collect(p.name) AS names
      RETURN [n IN names WHERE n STARTS WITH 'A'] AS filtered
      """
    Then the result should be, in any order:
      | filtered            |
      | ['Alice', 'Anna']   |

  Scenario: properties returns a map projection
    Given an empty graph
    And having executed:
      """
      MATCH (n) DETACH DELETE n;
      CREATE (:Person {name: 'Alice', age: 31});
      """
    When executing query:
      """
      MATCH (p:Person)
      RETURN properties(p) AS props
      """
    Then the result should be, in any order:
      | props                     |
      | {age: 31, name: 'Alice'}  |

  Scenario: unwind expands a literal list into rows
    Given any graph
    When executing query:
      """
      UNWIND [1, 2, 3] AS x
      RETURN x
      ORDER BY x
      """
    Then the result should be, in order:
      | x |
      | 1 |
      | 2 |
      | 3 |
