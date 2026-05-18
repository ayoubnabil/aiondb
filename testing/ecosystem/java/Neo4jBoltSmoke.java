import org.neo4j.driver.AuthTokens;
import org.neo4j.driver.Driver;
import org.neo4j.driver.GraphDatabase;
import org.neo4j.driver.Record;
import org.neo4j.driver.Result;
import org.neo4j.driver.Session;

public final class Neo4jBoltSmoke {
    public static void main(String[] args) {
        if (args.length != 3) {
            System.err.println("usage: Neo4jBoltSmoke <bolt-uri> <user> <password>");
            System.exit(2);
        }

        String uri = args[0];
        String user = args[1];
        String password = args[2];

        try (Driver driver = GraphDatabase.driver(uri, AuthTokens.basic(user, password));
             Session session = driver.session()) {
            Result result = session.run("RETURN 1 AS one, 'ok' AS status");
            if (!result.hasNext()) {
                throw new IllegalStateException("neo4j-java-driver returned no row for RETURN probe");
            }
            Record record = result.next();
            long one = record.get("one").asLong();
            String status = record.get("status").asString();
            if (one != 1L || !"ok".equals(status)) {
                throw new IllegalStateException(
                    "unexpected neo4j-java-driver payload: one=" + one + ", status=" + status
                );
            }

            System.out.println("{\"details\":\"Neo4j Java driver connected over Bolt and completed a read-only RETURN probe\",\"checks\":[\"bolt_connect\",\"auth\",\"session\",\"return_probe\"]}");
        } catch (Exception error) {
            System.out.println("{\"error\":" + quoteJson(error.getMessage()) + "}");
            System.exit(1);
        }
    }

    private static String quoteJson(String value) {
        if (value == null) {
            return "null";
        }
        StringBuilder out = new StringBuilder(value.length() + 2);
        out.append('"');
        for (int i = 0; i < value.length(); i++) {
            char ch = value.charAt(i);
            switch (ch) {
                case '"':
                    out.append("\\\"");
                    break;
                case '\\':
                    out.append("\\\\");
                    break;
                case '\b':
                    out.append("\\b");
                    break;
                case '\f':
                    out.append("\\f");
                    break;
                case '\n':
                    out.append("\\n");
                    break;
                case '\r':
                    out.append("\\r");
                    break;
                case '\t':
                    out.append("\\t");
                    break;
                default:
                    if (ch < 0x20) {
                        out.append(String.format("\\u%04x", (int) ch));
                    } else {
                        out.append(ch);
                    }
            }
        }
        out.append('"');
        return out.toString();
    }
}
