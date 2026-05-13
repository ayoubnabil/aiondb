Feature: Graph functions commonly used with Neo4j

  Scenario: type returns the relationship type
    Given an empty graph
    And having executed:
      """
      MATCH (n) DETACH DELETE n;
      CREATE (:Root)-[:KNOWS]->(:Person {name: 'Alice'});
      """
    When executing query:
      """
      MATCH (:Root)-[r]->(:Person)
      RETURN type(r) AS rel_type
      """
    Then the result should be, in any order:
      | rel_type |
      | 'KNOWS'  |

  Scenario: labels returns node labels
    Given an empty graph
    And having executed:
      """
      MATCH (n) DETACH DELETE n;
      CREATE (:Person:Employee {name: 'Alice'});
      """
    When executing query:
      """
      MATCH (n:Person)
      RETURN labels(n) AS node_labels
      """
    Then the result should be, in any order:
      | node_labels            |
      | ['Employee', 'Person'] |

  Scenario: length on a path returns relationship count
    Given an empty graph
    And having executed:
      """
      MATCH (n) DETACH DELETE n;
      CREATE (:Root)-[:KNOWS]->(:Person)-[:LIKES]->(:Topic);
      """
    When executing query:
      """
      MATCH p=(:Root)-[:KNOWS]->(:Person)-[:LIKES]->(:Topic)
      RETURN length(p) AS hops
      """
    Then the result should be, in any order:
      | hops |
      | 2    |

  Scenario: nodes and relationships expose path contents
    Given an empty graph
    And having executed:
      """
      MATCH (n) DETACH DELETE n;
      CREATE (:Root)-[:KNOWS]->(:Person)-[:LIKES]->(:Topic);
      """
    When executing query:
      """
      MATCH p=(:Root)-[:KNOWS]->(:Person)-[:LIKES]->(:Topic)
      RETURN size(nodes(p)) AS node_count, size(relationships(p)) AS rel_count
      """
    Then the result should be, in any order:
      | node_count | rel_count |
      | 3          | 2         |
