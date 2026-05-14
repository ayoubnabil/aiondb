Feature: Neo4j metadata procedures

  Scenario: db.labels returns labels in sorted order
    Given an empty graph
    And having executed:
      """
      MATCH (n) DETACH DELETE n;
      CREATE (:Person {name: 'Alice'})-[:KNOWS]->(:Person {name: 'Bob'});
      CREATE (:Movie {title: 'Matrix'});
      """
    When executing query:
      """
      CALL db.labels() YIELD label
      RETURN label
      ORDER BY label
      """
    Then the result should be, in order:
      | label    |
      | 'Movie'  |
      | 'Person' |

  Scenario: db.relationshipTypes returns relationship types
    Given an empty graph
    And having executed:
      """
      MATCH (n) DETACH DELETE n;
      CREATE (:Person)-[:KNOWS]->(:Person);
      CREATE (:Person)-[:LIKES]->(:Movie);
      """
    When executing query:
      """
      CALL db.relationshipTypes() YIELD relationshipType
      RETURN relationshipType
      ORDER BY relationshipType
      """
    Then the result should be, in order:
      | relationshipType |
      | 'KNOWS'          |
      | 'LIKES'          |

  Scenario: db.propertyKeys returns property keys
    Given an empty graph
    And having executed:
      """
      MATCH (n) DETACH DELETE n;
      CREATE (:Person {name: 'Alice', age: 31})-[:KNOWS {since: 2020}]->(:Person {name: 'Bob'});
      """
    When executing query:
      """
      CALL db.propertyKeys() YIELD propertyKey
      RETURN propertyKey
      ORDER BY propertyKey
      """
    Then the result should be, in order:
      | propertyKey |
      | 'age'       |
      | 'name'      |
      | 'since'     |
